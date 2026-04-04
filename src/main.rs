use abdb::parser::Parser;

fn main() {
    let sql = "SELECT name FROM products WHERE id IN (
        SELECT DISTINCT id FROM ORDERS
    )";
    let ast = Parser::parse(sql).unwrap();
    println!("{:#?}", ast);
    
    let sql = "SELECT c1, c2 FROM b1, b2 GROUP BY c2 HAVING AVG(c2) = 6";
    let ast = Parser::parse(sql).unwrap();
    println!("{:#?}", ast);

    let sql = "BEGIN;
    UPDATE accounts SET balance = balance - 100.00 WHERE name = 'Alice';
    UPDATE accounts SET balance = balance + 100.00 WHERE name = 'Bob';
    COMMIT;";
    let ast = Parser::parse(sql).unwrap();
    println!("{:#?}", ast);    
}
