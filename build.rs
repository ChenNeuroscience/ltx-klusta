use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=assets/ltxk.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ltxk.ico");
    let resource = Path::new(&env::var("OUT_DIR").unwrap()).join("icon.rc");
    fs::write(
        &resource,
        format!(
            "#pragma code_page(65001)\n1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("Cannot write the Windows icon resource");
    embed_resource::compile_for(&resource, ["ltxk"], embed_resource::NONE)
        .manifest_required()
        .expect("Cannot embed the Windows app icon");
}
