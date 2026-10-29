// rust-embed needs the folder to exist at compile time. When the web app has not
// been built yet, create a placeholder page so `cargo build` works from a clean
// checkout. Real builds run `pnpm -C web build` first (see images/allod.Dockerfile).
use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(&dist).expect("create web/dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=utf-8><title>Allod</title>\
             <p>Web UI not built. Run <code>pnpm -C web build</code>, then rebuild Allod.</p>",
        )
        .expect("write placeholder index.html");
    }
}
