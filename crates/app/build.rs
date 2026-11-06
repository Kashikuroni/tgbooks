fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=resources/windows/app.rc");
    println!("cargo:rerun-if-changed=resources/branding/app-icon.ico");
    #[cfg(windows)]
    embed_resource::compile_for(
        "resources/windows/app.rc",
        ["tgbooks"],
        embed_resource::ParamsIncludeDirs(["resources/windows", "resources/branding"]),
    )
    .manifest_required()?;
    Ok(())
}
