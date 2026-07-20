pub mod actions;
pub mod config;
pub mod errors;
pub mod http;
pub mod protocol;
pub mod resources;
pub mod server;
pub mod structures;
pub mod tls;
pub mod tools;
pub mod validation;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "MCP Filesystem Server")]
#[command(version)]
#[command(
    about = "High-performance Model Context Protocol filesystem App Server",
    long_about = None
)]
pub struct Args {
    /// Directories to allow access to (can specify multiple)
    #[arg(short, long)]
    pub directories: Vec<String>,

    /// Server host (used by the HTTP transport)
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    pub host: String,

    /// HTTP server port
    #[arg(long, visible_alias = "port", default_value = "3001")]
    pub http_port: u16,

    /// Log level
    #[arg(short, long, default_value = "info")]
    pub log_level: String,

    /// Maximum file size in MB for read operations
    #[arg(long, default_value = "100")]
    pub max_file_size: u64,

    /// Maximum decompressed output size in MB (guards against decompression bombs)
    #[arg(long, default_value = "1024")]
    pub max_decompressed_size: u64,

    /// Run in stdio mode for MCP compatibility
    #[arg(long)]
    pub stdio: bool,

    /// Print a local health payload and exit without starting a transport
    #[arg(long)]
    pub health: bool,

    /// Access mode: unrestricted or readonly
    #[arg(long, default_value = "unrestricted")]
    pub access_mode: config::AccessMode,

    /// Follow symbolic links
    #[arg(long)]
    pub follow_symlinks: bool,

    /// Request timeout in seconds
    #[arg(long, default_value = "30")]
    pub request_timeout: u64,

    /// Maximum size in bytes of a single JSON-RPC request line (stdio)
    #[arg(long, default_value = "16777216")]
    pub max_request_bytes: usize,

    /// Maximum size in bytes of a single HTTP JSON-RPC request body
    #[arg(long, default_value = "16777216")]
    pub max_http_body_bytes: usize,

    /// Optional bearer token required to access HTTP transports
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Path to a PEM certificate chain for HTTPS. Requires --tls-key.
    #[arg(long)]
    pub tls_cert: Option<String>,

    /// Path to the PEM private key matching --tls-cert.
    #[arg(long)]
    pub tls_key: Option<String>,

    /// Expose all tool categories.
    #[arg(long)]
    pub enable_all: bool,

    /// Enable read/list/search/hash tools.
    #[arg(long)]
    pub enable_read: bool,

    /// Enable write/edit/copy/move tools.
    #[arg(long)]
    pub enable_write: bool,

    /// Enable delete tools.
    #[arg(long)]
    pub enable_delete: bool,

    /// Enable compression tools.
    #[arg(long)]
    pub enable_compress: bool,

    /// Enable cryptography tools.
    #[arg(long)]
    pub enable_crypto: bool,

    /// Enable CSV tools.
    #[arg(long)]
    pub enable_csv: bool,
}

impl Args {
    pub fn enabled_categories(&self) -> Vec<tools::ToolCategory> {
        use tools::ToolCategory as Category;
        if self.enable_all {
            return Category::ALL.to_vec();
        }
        let mut categories = Vec::new();
        let mut push = |enabled: bool, category: Category| {
            if enabled {
                categories.push(category);
            }
        };
        push(self.enable_read, Category::Read);
        push(self.enable_write, Category::Write);
        push(self.enable_delete, Category::Delete);
        push(self.enable_compress, Category::Compress);
        push(self.enable_crypto, Category::Crypto);
        push(self.enable_csv, Category::Csv);
        categories
    }
}
