use autoreview_core::run_framework_checks;

#[test]
fn rabbitmq_acknowledgement_has_to_follow_successful_processing() {
    let repo = tempfile::tempdir().unwrap();
    let path = "consumer.ts";
    std::fs::write(repo.path().join(path), r#"
import * as rabbit from 'amqplib';

async function consume(url: string) {
    const connection = await rabbit.connect(url);
    const ch = await connection.createChannel();
    await ch.consume('orders', async message => {
        if (!message) return;
        try {
            await processOrder(message);
        } finally {
            ch.ack(message);
        }
    }, { noAck: false });
    await ch.consume('invoices', async delivery => {
        if (!delivery) return;
        try {
            await processInvoice(delivery);
            ch.ack(delivery);
        } catch {
            ch.nack(delivery, false, true);
        }
    });
    await ch.consume('events', async m => {
        try {
            await record(m);
        } finally {
            ch.ack(m);
        }
    }, { noAck: true });
}
"#).unwrap();
    let findings = run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].source.rule_id.as_deref(), Some("rabbitmq-ack-in-finally"));
    assert_eq!(findings[0].location.snippet, "ch.ack(message)");
    assert!(findings[0].related_locations.is_some());
}
