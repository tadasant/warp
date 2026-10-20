//! Serves a local AI Catalog + Server Card + MCP server for manual discovery demos.
//!
//! `cargo run -p ai_catalog --features test-util --example fixture_server -- [port] [bearer]`

use ai_catalog::test_support::FixtureCatalogServer;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let port = args
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or(8765);
    let bearer = args.next();
    let server = FixtureCatalogServer::start(port, bearer)?;
    println!("AI Catalog fixture domain serving at {}", server.origin());
    println!("  catalog: {}/.well-known/ai-catalog.json", server.origin());
    println!("  mcp:     {}", server.mcp_url());
    loop {
        std::thread::park();
    }
}
