use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

fn main() {
    let requested_path = env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("acpi.table"));
    let output_path = {
        let path = PathBuf::from(&requested_path);
        if path.is_absolute() {
            path
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
        }
    };
    let bytes = acpi_tables::qemu::qemu_q35_acpi_table();

    if let Err(err) = fs::write(&output_path, bytes) {
        eprintln!("failed to write {}: {err}", output_path.display());
        process::exit(1);
    }
}
