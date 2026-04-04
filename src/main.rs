// use abdb::parser::Parser;

// fn main() {
//     let sql = "SELECT name FROM products WHERE id IN (
//         SELECT DISTINCT id FROM ORDERS
//     )";
//     let ast = Parser::parse(sql).unwrap();
//     println!("{:#?}", ast);
    
//     let sql = "SELECT c1, c2 FROM b1, b2 GROUP BY c2 HAVING AVG(c2) = 6";
//     let ast = Parser::parse(sql).unwrap();
//     println!("{:#?}", ast);

//     let sql = "BEGIN;
//     UPDATE accounts SET balance = balance - 100.00 WHERE name = 'Alice';
//     UPDATE accounts SET balance = balance + 100.00 WHERE name = 'Bob';
//     COMMIT;";
//     let ast = Parser::parse(sql).unwrap();
//     println!("{:#?}", ast);    
// }

use tokio::net::TcpListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("Database server listening on 127.0.0.1:8080");

    loop {
        let (mut socket, addr) = listener.accept().await?;
        println!("New client connected: {}", addr);

        tokio::spawn(async move {
            // 1. Split the socket so we can read and write independently
            let (reader, mut writer) = socket.split();
            
            // 2. Wrap the reader in a BufReader to handle newline buffering
            let mut buf_reader = BufReader::new(reader);
            let mut query = String::new();

            loop {
                query.clear(); // Clear the buffer for the next query
                
                // 3. Read until we hit a newline (\n)
                match buf_reader.read_line(&mut query).await {
                    Ok(0) => {
                        println!("Client {} disconnected.", addr);
                        return; // Connection closed securely
                    }
                    Ok(_) => {
                        let sql = query.trim();
                        if sql.is_empty() { continue; } // Ignore empty lines

                        println!("Executing: {}", sql);

                        // 4. Send to your database engine
                        let result = process_sql(sql).await;

                        // 5. Send the result back to the client
                        if writer.write_all(result.as_bytes()).await.is_err() {
                            println!("Failed to write to client {}", addr);
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("Error reading from client {}: {}", addr, e);
                        return;
                    }
                }
            }
        });
    }
}

// Your mock database processing engine
async fn process_sql(sql: &str) -> String {
    // In reality, this would pass the string to your SQL parser,
    // generate an AST, hit the storage engine, and format the results.
    match sql.to_uppercase().as_str() {
        "SELECT 1;" => "RESULT: 1\n".to_string(),
        "SHOW TABLES;" => "TABLES: users, orders\n".to_string(),
        _ => format!("SYNTAX ERROR: Unknown or unsupported query '{}'\n", sql),
    }
}
