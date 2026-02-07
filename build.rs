fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    println!("cargo:rerun-if-changed=proto/lightnode.proto");
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["proto/lightnode.proto"], &["proto"])?;

    Ok(())
}
