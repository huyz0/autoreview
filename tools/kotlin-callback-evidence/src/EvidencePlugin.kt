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
import org.jetbrains.kotlin.ir.declarations.IrFunction
import org.jetbrains.kotlin.ir.declarations.IrSimpleFunction
import org.jetbrains.kotlin.ir.declarations.IrDeclarationOrigin
import org.jetbrains.kotlin.ir.declarations.IrParameterKind
import org.jetbrains.kotlin.ir.expressions.IrCall
import org.jetbrains.kotlin.ir.expressions.IrFunctionExpression
import org.jetbrains.kotlin.ir.expressions.IrFunctionReference
import org.jetbrains.kotlin.ir.expressions.IrPropertyReference
import org.jetbrains.kotlin.ir.expressions.IrSetField
import org.jetbrains.kotlin.ir.expressions.IrConst
import org.jetbrains.kotlin.ir.declarations.IrClass
import org.jetbrains.kotlin.ir.expressions.IrTypeOperator
import org.jetbrains.kotlin.ir.expressions.IrTypeOperatorCall
import org.jetbrains.kotlin.ir.symbols.UnsafeDuringIrConstructionAPI
import org.jetbrains.kotlin.ir.symbols.IrPropertySymbol
import org.jetbrains.kotlin.ir.types.classFqName
import org.jetbrains.kotlin.ir.types.IrSimpleType
import org.jetbrains.kotlin.ir.util.fqNameWhenAvailable
import org.jetbrains.kotlin.ir.util.fileOrNull
import org.jetbrains.kotlin.ir.visitors.IrVisitorVoid
import org.jetbrains.kotlin.ir.visitors.acceptChildrenVoid

private const val MAX_SOURCE_BYTES = 4 * 1024 * 1024
private const val MAX_TOTAL_SOURCE_BYTES = 16 * 1024 * 1024
private const val MAX_FILES = 512
private const val MAX_RECORDS = 20_000
private const val MAX_PROPERTY_SETTERS = 512
private const val MAX_OUTPUT_BYTES = 1024 * 1024
private const val JSON_NULL = "null"

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
    val receiverProperty: String?,
    val observedSetterCallees: List<String?>?,
    val observedSetterDetails: List<SetterDetail?>?,
    val propertySourceSha256: String?,
    val propertyEvidence: PropertyEvidence?,
)

private data class PropertyEvidence(
    val visibility: String,
    val defaultGetter: Boolean,
    val defaultSetter: Boolean,
    val nullInitializer: Boolean,
    val directFieldWrites: Int,
    val accessorReferences: Int,
)

private data class PropertyObservations(
    val setters: Map<IrPropertySymbol, List<SetterDetail?>>,
    val directWrites: Map<IrPropertySymbol, Int>,
    val references: Map<IrPropertySymbol, Int>,
    val sourceHashes: Map<String, String>,
)

private data class SetterDetail(
    val callee: String,
    val origin: String,
    val dispatchReceiver: Boolean,
    val parameterTypes: List<String?>,
    val returnType: String?,
    val typeParameters: Int,
)

@OptIn(UnsafeDuringIrConstructionAPI::class)
internal class EvidenceExtension : IrGenerationExtension {
    override fun generate(moduleFragment: IrModuleFragment, pluginContext: IrPluginContext) {
        check(KotlinCompilerVersion.VERSION in setOf("2.2.20", "2.3.21")) { "unsupported Kotlin compiler version" }
        val output = System.getenv("AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT")
            ?: error("AUTOREVIEW_CALLBACK_EVIDENCE_OUTPUT is required")
        val records = mutableListOf<Record>()
        require(moduleFragment.files.size <= MAX_FILES) { "too many source files" }
        val properties = mutableSetOf<IrPropertySymbol>()
        val sourceClasses = mutableSetOf<String>()
        moduleFragment.acceptChildrenVoid(object : IrVisitorVoid() {
            override fun visitElement(element: IrElement) {
                if (element is IrClass) {
                    element.fqNameWhenAvailable?.asString()?.let { sourceClasses.add(it) }
                    require(sourceClasses.size <= MAX_RECORDS) { "too many source classes" }
                }
                if (element is IrCall && element.arguments.any { it is IrTypeOperatorCall && it.operator == IrTypeOperator.SAM_CONVERSION }) {
                    val receiverIndex = element.symbol.owner.parameters.indexOfFirst { it.kind == IrParameterKind.DispatchReceiver }
                    receiverProperty(element.arguments.getOrNull(receiverIndex) as? IrCall)?.let { properties.add(it) }
                    require(properties.size <= MAX_RECORDS) { "too many receiver properties" }
                }
                element.acceptChildrenVoid(this)
            }
        })
        val setters = mutableMapOf<IrPropertySymbol, MutableList<SetterDetail?>>()
        val fields = properties.mapNotNull { property -> property.owner.backingField?.symbol?.let { it to property } }.toMap()
        val directWrites = mutableMapOf<IrPropertySymbol, Int>()
        val references = mutableMapOf<IrPropertySymbol, Int>()
        var setterCount = 0
        moduleFragment.acceptChildrenVoid(object : IrVisitorVoid() {
            private var function: IrFunction? = null
            override fun visitFunction(declaration: IrFunction) {
                val previous = function
                function = declaration
                declaration.acceptChildrenVoid(this)
                function = previous
            }
            override fun visitElement(element: IrElement) {
                if (element is IrCall && collectSetter(element, properties, setters)) {
                    setterCount++
                    require(setterCount <= MAX_RECORDS) { "too many property setter calls" }
                }
                if (element is IrSetField) {
                    val property = fields[element.symbol]
                    if (property != null && (function?.symbol != property.owner.setter?.symbol || function?.origin != IrDeclarationOrigin.DEFAULT_PROPERTY_ACCESSOR)) {
                        directWrites[property] = (directWrites[property] ?: 0) + 1
                    }
                }
                if (element is IrFunctionReference) {
                    val property = (element.symbol.owner as? IrSimpleFunction)?.correspondingPropertySymbol
                    if (property != null && property in properties) references[property] = (references[property] ?: 0) + 1
                }
                if (element is IrPropertyReference && element.symbol in properties) {
                    references[element.symbol] = (references[element.symbol] ?: 0) + 1
                }
                element.acceptChildrenVoid(this)
            }
        })
        var totalSourceBytes = 0
        val digest = MessageDigest.getInstance("SHA-256")
        val sourceHashes = moduleFragment.files.associate { file ->
            val path = Path.of(file.fileEntry.name).toAbsolutePath().normalize()
            val bytes = Files.newInputStream(path).use { it.readNBytes(MAX_SOURCE_BYTES + 1) }
            require(bytes.size <= MAX_SOURCE_BYTES) { "source exceeds evidence limit" }
            totalSourceBytes += bytes.size
            require(totalSourceBytes <= MAX_TOTAL_SOURCE_BYTES) { "total source exceeds evidence limit" }
            val hash = digest.digest(bytes).joinToString("") { "%02x".format(it) }
            path.toString() to hash
        }
        val observations = PropertyObservations(setters = setters, directWrites = directWrites, references = references, sourceHashes = sourceHashes)
        for (file in moduleFragment.files) {
            val path = Path.of(file.fileEntry.name).toAbsolutePath().normalize()
            val hash = sourceHashes.getValue(path.toString())
            file.acceptChildrenVoid(object : IrVisitorVoid() {
                override fun visitElement(element: IrElement) {
                    if (element is IrCall) collect(call = element, path = path.toString(), hash = hash, records = records, observations = observations)
                    element.acceptChildrenVoid(this)
                }
            })
        }
        val json = encode(records, sourceClasses.sorted())
        val bytes = json.toByteArray(StandardCharsets.UTF_8)
        require(bytes.size <= MAX_OUTPUT_BYTES) { "evidence output exceeds limit" }
        Files.write(Path.of(output), bytes, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)
    }
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun collect(call: IrCall, path: String, hash: String, records: MutableList<Record>, observations: PropertyObservations) {
    val owner = call.symbol.owner
    val callee = owner.fqNameWhenAvailable?.asString() ?: return
    if (call.startOffset < 0 || call.endOffset < call.startOffset) return
    if (call.arguments.size != owner.parameters.size) return
    val receiverIndex = owner.parameters.indexOfFirst { it.kind == IrParameterKind.DispatchReceiver }
    val receiver = call.arguments.getOrNull(receiverIndex)
    val receiverStaticType = receiver?.type?.classFqName?.asString()
    val property = receiverProperty(receiver as? IrCall)
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
            receiverProperty = property?.owner?.fqNameWhenAvailable?.asString(),
            observedSetterCallees = property?.let { observations.setters[it]?.map { detail -> detail?.callee } ?: emptyList() },
            observedSetterDetails = property?.let { observations.setters[it]?.toList() ?: emptyList() },
            propertySourceSha256 = property?.owner?.fileOrNull?.fileEntry?.name?.let {
                observations.sourceHashes[Path.of(it).toAbsolutePath().normalize().toString()]
            },
            propertyEvidence = property?.let { propertyEvidence(it, observations) },
        ))
    }
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun propertyEvidence(symbol: IrPropertySymbol, observations: PropertyObservations): PropertyEvidence {
    val property = symbol.owner
    val initializer = property.backingField?.initializer?.expression as? IrConst
    return PropertyEvidence(
        visibility = property.visibility.toString(),
        defaultGetter = property.getter?.origin == IrDeclarationOrigin.DEFAULT_PROPERTY_ACCESSOR,
        defaultSetter = property.setter?.origin == IrDeclarationOrigin.DEFAULT_PROPERTY_ACCESSOR,
        nullInitializer = initializer != null && initializer.value == null,
        directFieldWrites = observations.directWrites[symbol] ?: 0,
        accessorReferences = observations.references[symbol] ?: 0,
    )
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun receiverProperty(call: IrCall?): IrPropertySymbol? {
    val actual = if (call?.symbol?.owner?.fqNameWhenAvailable?.asString() == "kotlin.internal.ir.CHECK_NOT_NULL") {
        call.arguments.singleOrNull() as? IrCall
    } else call
    val function = actual?.symbol?.owner ?: return null
    val property = function.correspondingPropertySymbol ?: return null
    return property.takeIf { it.owner.getter?.symbol == function.symbol }
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun collectSetter(call: IrCall, properties: Set<IrPropertySymbol>, setters: MutableMap<IrPropertySymbol, MutableList<SetterDetail?>>): Boolean {
    val function = call.symbol.owner
    val property = function.correspondingPropertySymbol ?: return false
    if (property !in properties) return false
    if (property.owner.setter?.symbol != function.symbol || call.arguments.size != function.parameters.size) return false
    val arguments = function.parameters.zip(call.arguments).filter { (parameter, _) -> parameter.kind == IrParameterKind.Regular }
    if (arguments.size != 1) return false
    val value = arguments.single().second as? IrCall
    val calls = setters.getOrPut(property) { mutableListOf() }
    check(calls.size < MAX_PROPERTY_SETTERS) { "too many calls to one property setter" }
    calls.add(value?.let(::setterDetail))
    return true
}

@OptIn(UnsafeDuringIrConstructionAPI::class)
private fun setterDetail(call: IrCall): SetterDetail? {
    val function = call.symbol.owner
    val name = function.fqNameWhenAvailable?.asString() ?: return null
    if (call.arguments.size != function.parameters.size) return null
    return SetterDetail(
        callee = name, origin = function.origin.toString(),
        dispatchReceiver = function.parameters.any { it.kind == IrParameterKind.DispatchReceiver || it.kind == IrParameterKind.ExtensionReceiver },
        parameterTypes = function.parameters.filter { it.kind == IrParameterKind.Regular }.map { it.type.classFqName?.asString() },
        returnType = function.returnType.classFqName?.asString(), typeParameters = function.typeParameters.size,
    )
}

private fun encode(records: List<Record>, sourceClasses: List<String>): String = buildString {
    append("{\"version\":2,\"compiler\":").append(quote(KotlinCompilerVersion.VERSION)).append(",\"sourceClasses\":[")
    sourceClasses.forEachIndexed { index, name ->
        if (index != 0) append(',')
        append(quote(name))
    }
    append("],\"records\":[")
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
        append(",\"receiverStaticType\":").append(record.receiverStaticType?.let(::quote) ?: JSON_NULL)
        append(",\"receiverProperty\":").append(record.receiverProperty?.let(::quote) ?: JSON_NULL)
        append(",\"observedSetterCallees\":")
        if (record.observedSetterCallees == null) append(JSON_NULL) else {
            append('[')
            record.observedSetterCallees.forEachIndexed { setterIndex, callee ->
                if (setterIndex != 0) append(',')
                append(callee?.let(::quote) ?: JSON_NULL)
            }
            append(']')
        }
        append(",\"propertySourceSha256\":").append(record.propertySourceSha256?.let(::quote) ?: JSON_NULL)
        append(",\"observedSetterDetails\":")
        if (record.observedSetterDetails == null) append(JSON_NULL) else {
            append('[')
            record.observedSetterDetails.forEachIndexed { detailIndex, detail ->
                if (detailIndex != 0) append(',')
                if (detail == null) append(JSON_NULL) else appendSetterDetail(detail)
            }
            append(']')
        }
        append(",\"propertyEvidence\":")
        val property = record.propertyEvidence
        if (property == null) append(JSON_NULL) else {
            append("{\"visibility\":").append(quote(property.visibility))
            append(",\"defaultGetter\":").append(property.defaultGetter)
            append(",\"defaultSetter\":").append(property.defaultSetter)
            append(",\"nullInitializer\":").append(property.nullInitializer)
            append(",\"directFieldWrites\":").append(property.directFieldWrites)
            append(",\"accessorReferences\":").append(property.accessorReferences).append('}')
        }
        append(",\"parameterTypes\":[")
        record.parameterTypes.forEachIndexed { typeIndex, type ->
            if (typeIndex != 0) append(',')
            append(type?.let(::quote) ?: JSON_NULL)
        }
        append("]}")
    }
    append("]}")
}

private fun StringBuilder.appendSetterDetail(detail: SetterDetail) {
    append("{\"callee\":").append(quote(detail.callee))
    append(",\"origin\":").append(quote(detail.origin))
    append(",\"dispatchReceiver\":").append(detail.dispatchReceiver)
    append(",\"parameterTypes\":[")
    detail.parameterTypes.forEachIndexed { index, type ->
        if (index != 0) append(',')
        append(type?.let(::quote) ?: JSON_NULL)
    }
    append("],\"returnType\":").append(detail.returnType?.let(::quote) ?: JSON_NULL)
    append(",\"typeParameters\":").append(detail.typeParameters).append('}')
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
