use autoreview_core::run_framework_checks;

#[test]
fn prisma_reports_writes_through_the_same_outer_client_and_its_module_aliases() {
    let repo = tempfile::tempdir().unwrap();
    let path = "db.ts";
    std::fs::write(repo.path().join(path), r#"
import { PrismaClient as Client } from '@prisma/client';
const prisma = new Client();
const db = prisma;
const other = new Client();

export async function save() {
    await db.$transaction(async (tx) => {
        await tx.user.create({ data: { name: 'Ada' } });
        const trx = tx;
        await trx.user.update({ where: { id: 1 }, data: { name: 'Ada' } });
        if (enabled) {
            await prisma.user.delete({ where: { id: 2 } });
        }
        await other.user.create({ data: { name: 'Kim' } });
        return await db.user.findMany();
    });
    await prisma.user.update({ where: { id: 2 }, data: { active: false } });
}
"#).unwrap();
    let findings = run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].source.rule_id.as_deref(), Some("prisma-outer-client-write-in-transaction"));
    assert!(findings[0].location.snippet.starts_with("prisma.user.delete"));
    assert!(findings[0].related_locations.as_ref().unwrap()[0].snippet.starts_with("db.$transaction"));
}

#[test]
fn prisma_ignores_callable_objects_of_unknown_origin_and_shadowed_receivers() {
    let repo = tempfile::tempdir().unwrap();
    let path = "db.js";
    std::fs::write(repo.path().join(path), r#"
import { PrismaClient } from '@prisma/client';
const db = new PrismaClient();
const service = new Fake();
async function save() {
    await db.$transaction(async (db) => {
        await db.user.create({});
    });
    await service.$transaction(async () => {
        await service.user.create({});
    });
}
"#).unwrap();
    assert!(run_framework_checks(repo.path(), &[path.into()]).is_empty());
}
