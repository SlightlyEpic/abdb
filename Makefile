abdb:
	RUSTFLAGS=-Awarnings cargo run --bin abdb

# client:
# 	RUSTFLAGS=-Awarnings cargo run --bin client

client:
	rlwrap nc localhost 8080
