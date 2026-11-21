use actix_web::HttpRequest;

fn search_statement(request: HttpRequest) {
    let query = request.query_string();
    let _statement = sqlx::raw_sql(query);
}
