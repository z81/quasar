fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file_names = [
        "protos/alice/protos/api/alicekit/protocol/client/client_message.proto".to_string(),
        "protos/alice/protos/api/alicekit/protocol/server/server_message.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/alarm/capability.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/animation/capability.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/iot_discovery/capability.proto".to_string(),
        "protos/alice/protos/api/typed_callbacks/iot/iot.proto".to_string(),
        "protos/alice/protos/api/typed_callbacks/typed_callback_request.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/on_off/capability.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/level/capability.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/equalizer/capability.proto".to_string(),
        "protos/alice/protos/endpoint/capabilities/do_not_disturb/capability.proto".to_string(),
    ];

    let file_descriptors = protox::compile(file_names, ["protos/".to_string()])?;

    let mut config = prost_build::Config::new();
    config.compile_well_known_types();
    config.skip_protoc_run();
    config.compile_fds(file_descriptors)?;

    println!("cargo:rerun-if-changed=protos/");
    Ok(())
}
