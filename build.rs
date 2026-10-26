fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file_names = [
        "protos/alice/protos/api/alicekit/protocol/client/client_message.proto".to_string(),
        "protos/alice/protos/api/alicekit/protocol/server/server_message.proto".to_string(),
    ];

    let file_descriptors = protox::compile(file_names, ["protos/".to_string()])?;

    // the same schemas for runtime reflection (the station's device state goes
    // to remote servers as JSON); comments are only needed for codegen
    let mut runtime_descriptors = file_descriptors.clone();
    for file in &mut runtime_descriptors.file {
        file.source_code_info = None;
    }
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(
        out_dir.join("alice_descriptors.bin"),
        protox::prost::Message::encode_to_vec(&runtime_descriptors),
    )?;

    let mut config = prost_build::Config::new();
    config.compile_well_known_types();
    config.skip_protoc_run();
    config.compile_fds(file_descriptors)?;

    println!("cargo:rerun-if-changed=protos/");
    Ok(())
}
