package autoreview.callbackevidence

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption
import java.security.MessageDigest
import org.jetbrains.kotlin.backend.common.extensions.IrGenerationExtension
import org.jetbrains.kotlin.backend.common.extensions.IrPluginContext
import org.jetbrains.kotlin.config.KotlinCompilerVersion
import org.jetbrains.kotlin.ir.IrElement
import org.jetbrains.kotlin.ir.declarations.IrModuleFragment
import org.jetbrains.kotlin.ir.declarations.IrParameterKind
import org.jetbrains.kotlin.ir.expressions.IrCall
import org.jetbrains.kotlin.ir.expressions.IrFunctionExpression
import org.jetbrains.kotlin.ir.expressions.IrTypeOperator
import org.jetbrains.kotlin.ir.expressions.IrTypeOperatorCall
import org.jetbrains.kotlin.ir.symbols.UnsafeDuringIrConstructionAPI
import org.jetbrains.kotlin.ir.types.classFqName
import org.jetbrains.kotlin.ir.types.IrSimpleType
import org.jetbrains.kotlin.ir.util.fqNameWhenAvailable
import org.jetbrains.kotlin.ir.visitors.IrVisitorVoid
import org.jetbrains.kotlin.ir.visitors.acceptChildrenVoid

private const val MAX_SOURCE_BYTES = 4 * 1024 * 1024
private const val MAX_TOTAL_SOURCE_BYTES = 16 * 1024 * 1024
private const val MAX_FILES = 512
private const val MAX_RECORDS = 20_000
private const val MAX_OUTPUT_BYTES = 1024 * 1024

private data class Record(
    val path: String,
    val sha256: String,
    val callStartUtf16: Int,
    val callEndUtf16: Int,
    val lambdaStartUtf16: Int,
    val lambdaEndUtf16: Int,
    val callee: String,
    val parameter: Int,
    val samType: String,
    val modality: String,
    val parameterTypes: List<String?>,
    val receiverStaticType: String?,
)

@OptIn(UnsafeDuringIrConstructionAPI::class)
internal class EvidenceExtension : IrGenerationExtension {
    override fun generate(moduleFragment: IrModuleFragment, pluginContext: IrPluginContext) {
        check(KotlinCompilerVersion.VERSION in setOf("2.2.20", "2.3.21")) { "unsupported Kotlin compiler version" }
        val output = System.getenv("AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT")
            ?: error("AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT is required")
        val records = mutableListOf<Record>()
        require(moduleFragment.files.size <= MAX_FILES) { "too many source files" }
        var totalSourceBytes = 0
        val digest = MessageDigest.getInstance("SHA-256")
        for (file in moduleFragment.files) {
            val path = Path.of(file.fileEntry.name).toAbsolutePath().normalize()
            val bytes = Files.newInputStream(path).use { it.readNBytes(MAX_SOURCE_BYTES + 1) }
            require(bytes.size <= MAX_SOURCE_BYTES) { "source exceeds evidence limit" }
            totalSourceBytes += bytes.size
            require(totalSourceBytes <= MAX_TOTAL_SOURCE_BYTES) { "total source exceeds evidence limit" }
            val hash = digest.digest(bytes).joinToString("") { "%02x".format(it) }
            file.acceptChildrenVoid(object : IrVisitorVoid() {
                override fun visitElement(element: IrElement) {
                    if (element is IrCall) collect(call = element, path = path.toString(), hash = hash, records = records)
                    element.acceptChildrenVoid(this)
                }
            })
        }
        val json = encode(records)
        val bytes = json.toByteArray(StandardCharsets.UTF_8)
        require(bytes.size <= MAX_OUTPUT_BYTES) { "evidence output exceeds limit" }
        Files.write(Path.of(output), bytes, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)
    }
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun collect(call: IrCall, path: String, hash: String, records: MutableList<Record>) {
    val owner = call.symbol.owner
    val callee = owner.fqNameWhenAvailable?.asString() ?: return
    if (call.startOffset < 0 || call.endOffset < call.startOffset) return
    if (call.arguments.size != owner.parameters.size) return
    val receiverIndex = owner.parameters.indexOfFirst { it.kind == IrParameterKind.DispatchReceiver }
    val receiverStaticType = call.arguments.getOrNull(receiverIndex)?.type?.classFqName?.asString()
    var regularIndex = 0
    for ((parameter, argument) in owner.parameters.zip(call.arguments)) {
        if (parameter.kind != IrParameterKind.Regular) continue
        val index = regularIndex++
        val conversion = argument as? IrTypeOperatorCall ?: continue
        if (conversion.operator != IrTypeOperator.SAM_CONVERSION) continue
        val lambda = conversion.argument as? IrFunctionExpression ?: continue
        val samType = conversion.typeOperand.classFqName?.asString() ?: continue
        if (lambda.startOffset < 0 || lambda.endOffset < lambda.startOffset) continue
        check(records.size < MAX_RECORDS) { "too many callback evidence records" }
        records.add(Record(
            path = path, sha256 = hash, callStartUtf16 = call.startOffset, callEndUtf16 = call.endOffset,
            lambdaStartUtf16 = lambda.startOffset, lambdaEndUtf16 = lambda.endOffset,
            callee = callee, parameter = index, samType = samType, modality = owner.modality.name,
            parameterTypes = owner.parameters.filter { it.kind == IrParameterKind.Regular }.map {
                val type = it.type as? IrSimpleType
                if (type == null || type.arguments.isNotEmpty()) null else type.classFqName?.asString()
            },
            receiverStaticType = receiverStaticType,
        ))
    }
}

private fun encode(records: List<Record>): String = buildString {
    append("{\"version\":2,\"compiler\":").append(quote(KotlinCompilerVersion.VERSION)).append(",\"records\":[")
    records.forEachIndexed { index, record ->
        if (index != 0) append(',')
        append("{\"path\":").append(quote(record.path))
        append(",\"sha256\":").append(quote(record.sha256))
        append(",\"callStartUtf16\":").append(record.callStartUtf16)
        append(",\"callEndUtf16\":").append(record.callEndUtf16)
        append(",\"lambdaStartUtf16\":").append(record.lambdaStartUtf16)
        append(",\"lambdaEndUtf16\":").append(record.lambdaEndUtf16)
        append(",\"callee\":").append(quote(record.callee))
        append(",\"parameter\":").append(record.parameter)
        append(",\"samType\":").append(quote(record.samType))
        append(",\"modality\":").append(quote(record.modality))
        append(",\"receiverStaticType\":").append(record.receiverStaticType?.let(::quote) ?: "null")
        append(",\"parameterTypes\":[")
        record.parameterTypes.forEachIndexed { typeIndex, type ->
            if (typeIndex != 0) append(',')
            append(type?.let(::quote) ?: "null")
        }
        append("]}")
    }
    append("]}")
}

private fun quote(value: String): String = buildString {
    append('"')
    for (character in value) {
        when (character) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            in '\u0000'..'\u001f' -> append("\\u%04x".format(character.code))
            in '\ud800'..'\udfff' -> append("\\u%04x".format(character.code))
            else -> append(character)
        }
    }
    append('"')
}
