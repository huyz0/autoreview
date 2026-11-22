use autoreview_core::run_framework_checks;

#[test]
fn database_sql_follows_live_transactions_and_receiver_aliases() {
    let repo = tempfile::tempdir().unwrap();
    let path = "orders.go";
    std::fs::write(repo.path().join(path), r#"
package main
import sq "database/sql"

func save(db, other *sq.DB, direct bool) error {
    primary := db
    tx, startErr := primary.Begin()
    if startErr != nil {
        db.Exec("record begin failure")
        return startErr
    }
    defer tx.Rollback()
    trx := tx
    trx.Exec("within")
    other.Exec("independent")
    if direct {
        return trx.Commit()
    }
    if !direct {
        primary.Exec("outside")
    }
    if err := trx.Commit(); err != nil { return err }
    return nil
}

func after(db *sq.DB) {
    tx, _ := db.Begin()
    alias := tx
    alias.Commit()
    db.Exec("after")
}

func reassigned(db, other *sq.DB) {
    tx, _ := db.Begin()
    db = other
    db.Exec("separate")
    tx.Rollback()
}

func switched(db, other *sq.DB, yes bool) {
    tx, err := db.Begin()
    if err == nil {
        db := other
        db.Exec("separate")
        if yes {
            tx.Commit()
        } else {
            tx.Rollback()
        }
    }
    db.Exec("after")
}
"#).unwrap();
    let findings = run_framework_checks(repo.path(), &[path.into()]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].source.rule_id.as_deref(), Some("go-sql-db-write-during-transaction"));
    assert_eq!(findings[0].location.snippet, "primary.Exec(\"outside\")");
    assert_eq!(findings[0].related_locations.as_ref().unwrap()[0].snippet, "primary.Begin()");
    assert_eq!(
        findings[0].meta.as_ref().unwrap()["subcategory"],
        serde_json::json!(["persistence.transaction-boundaries", "persistence.data-integrity"])
    );
}
