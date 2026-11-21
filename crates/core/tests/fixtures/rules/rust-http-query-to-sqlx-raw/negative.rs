use actix_web::HttpRequest;

fn search_statement(request: HttpRequest) {
    let value = request.query_string();
    let _statement = sqlx::query("SELECT * FROM items WHERE name = $1").bind(value);
}
