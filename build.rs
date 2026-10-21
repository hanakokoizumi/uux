fn main() {
    println!("cargo:rerun-if-changed=proto");
    prost_build::Config::new()
        .compile_protos(&["proto/main.proto"], &["proto"])
        .expect("compile recovered UU wire schema (requires protoc)");
}
