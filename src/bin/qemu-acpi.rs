use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    process,
};

use acpi_tables::qemu::{qemu_q35_acpi_table_from_profile, QemuAcpiError, QemuQ35AcpiProfile};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Config {
    meta: Meta,
    platform: Platform,
    cpu: Cpu,
    #[serde(default)]
    memory: Memory,
    #[serde(default)]
    machine: Machine,
    #[serde(default)]
    acpi: Acpi,
    #[serde(default)]
    netdevs: Vec<Netdev>,
    #[serde(default)]
    devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    profile: String,
}

#[derive(Debug, Deserialize)]
struct Platform {
    arch: String,
    machine: String,
}

#[derive(Debug, Deserialize)]
struct Cpu {
    topology: CpuTopology,
}

#[derive(Debug, Deserialize)]
struct CpuTopology {
    cpus: u8,
    maxcpus: u8,
}

#[derive(Debug, Default, Deserialize)]
struct Memory {
    #[serde(default)]
    size: String,
    #[serde(default)]
    backend: String,
    #[serde(default)]
    memory_encryption: String,
    #[serde(default)]
    aux_ram_share: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Machine {
    #[serde(default)]
    properties: MachineProperties,
    #[serde(default)]
    features: MachineFeatures,
}

#[derive(Debug, Default, Deserialize)]
struct MachineProperties {
    #[serde(default)]
    hpet: bool,
    #[serde(default)]
    smm: bool,
    #[serde(default)]
    pic: bool,
    #[serde(default)]
    spcr: bool,
    #[serde(default)]
    nvdimm: bool,
    #[serde(default)]
    hmat: bool,
}

#[derive(Debug, Default, Deserialize)]
struct MachineFeatures {
    #[serde(default)]
    confidential_guest: bool,
    #[serde(default)]
    cxl: bool,
    #[serde(default)]
    sgx_epc: bool,
    #[serde(default)]
    tpm: bool,
    #[serde(default)]
    viommu: bool,
}

#[derive(Debug, Default, Deserialize)]
struct Acpi {
    #[serde(default)]
    tables: Vec<AcpiTable>,
}

#[derive(Debug, Deserialize)]
struct AcpiTable {
    kind: String,
    signature: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Netdev {
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Device {
    driver: Option<String>,
    id: Option<String>,
}

#[derive(Debug)]
enum CliError {
    Usage(String),
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseToml(toml::de::Error),
    Unsupported(String),
    WriteOutput {
        path: PathBuf,
        source: std::io::Error,
    },
    Generate(QemuAcpiError),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(msg) => write!(f, "{msg}"),
            CliError::ReadConfig { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            CliError::ParseToml(source) => write!(f, "failed to parse TOML: {source}"),
            CliError::Unsupported(msg) => write!(f, "{msg}"),
            CliError::WriteOutput { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            CliError::Generate(err) => write!(f, "{err}"),
        }
    }
}

fn usage(program: &str) -> String {
    format!("usage: {program} <config.toml> [-o output]")
}

fn parse_args(args: &[String]) -> Result<(PathBuf, PathBuf), CliError> {
    if args.len() < 2 {
        return Err(CliError::Usage(usage(&args[0])));
    }

    let input = PathBuf::from(&args[1]);
    let mut output = PathBuf::from("acpi.table");
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                if index >= args.len() {
                    return Err(CliError::Usage(usage(&args[0])));
                }
                output = PathBuf::from(&args[index]);
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unexpected argument: {other}\n{}",
                    usage(&args[0])
                )));
            }
        }
        index += 1;
    }

    Ok((input, output))
}

fn require_false(name: &str, value: bool) -> Result<(), CliError> {
    if value {
        return Err(CliError::Unsupported(format!(
            "{name} = true is not supported by the current static q35 ACPI template"
        )));
    }
    Ok(())
}

fn validate_config(config: &Config) -> Result<QemuQ35AcpiProfile, CliError> {
    if config.meta.profile != "q35-ovmf-static-acpi" {
        return Err(CliError::Unsupported(format!(
            "unsupported meta.profile {:?}; expected \"q35-ovmf-static-acpi\"",
            config.meta.profile
        )));
    }
    if config.platform.arch != "x86_64" {
        return Err(CliError::Unsupported(format!(
            "unsupported platform.arch {:?}; expected \"x86_64\"",
            config.platform.arch
        )));
    }
    if config.platform.machine != "q35" {
        return Err(CliError::Unsupported(format!(
            "unsupported platform.machine {:?}; expected \"q35\"",
            config.platform.machine
        )));
    }

    require_false("machine.properties.hpet", config.machine.properties.hpet)?;
    require_false("machine.properties.smm", config.machine.properties.smm)?;
    require_false("machine.properties.pic", config.machine.properties.pic)?;
    require_false("machine.properties.spcr", config.machine.properties.spcr)?;
    require_false(
        "machine.properties.nvdimm",
        config.machine.properties.nvdimm,
    )?;
    require_false("machine.properties.hmat", config.machine.properties.hmat)?;
    require_false(
        "machine.features.confidential_guest",
        config.machine.features.confidential_guest,
    )?;
    require_false("machine.features.cxl", config.machine.features.cxl)?;
    require_false("machine.features.sgx_epc", config.machine.features.sgx_epc)?;
    require_false("machine.features.tpm", config.machine.features.tpm)?;
    require_false("machine.features.viommu", config.machine.features.viommu)?;

    if !config.acpi.tables.is_empty() {
        let table = &config.acpi.tables[0];
        return Err(CliError::Unsupported(format!(
            "acpi.tables overrides are not supported yet (first entry: kind={:?}, signature={:?}, source={:?})",
            table.kind, table.signature, table.source
        )));
    }

    // Memory settings are intentionally parsed but ignored for the current
    // TDX/q35 static ACPI profile. Changing RAM size alone does not affect the
    // generated ACPI blob unless future NUMA/HMAT/NVDIMM-like features are added.
    let _ = (
        &config.memory.size,
        &config.memory.backend,
        &config.memory.memory_encryption,
        config.memory.aux_ram_share,
    );

    for netdev in &config.netdevs {
        let _ = (&netdev.id, &netdev.kind);
    }
    for device in &config.devices {
        let _ = (&device.driver, &device.id);
    }

    Ok(QemuQ35AcpiProfile {
        cpu_count: config.cpu.topology.cpus,
        max_cpu_count: config.cpu.topology.maxcpus,
    })
}

fn load_config(path: &Path) -> Result<Config, CliError> {
    let raw = fs::read_to_string(path).map_err(|source| CliError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&raw).map_err(CliError::ParseToml)
}

fn run() -> Result<(), CliError> {
    let args: Vec<String> = env::args().collect();
    let (input, output) = parse_args(&args)?;
    let config = load_config(&input)?;
    let profile = validate_config(&config)?;
    let bytes = qemu_q35_acpi_table_from_profile(profile).map_err(CliError::Generate)?;
    fs::write(&output, bytes).map_err(|source| CliError::WriteOutput {
        path: output,
        source,
    })?;
    Ok(())
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_CONFIG: &str = r#"
[meta]
schema_version = 1
profile = "q35-ovmf-static-acpi"

[platform]
arch = "x86_64"
machine = "q35"

[cpu]
model = "host"

[cpu.topology]
cpus = 4
maxcpus = 4
"#;

    const SAMPLE_CONFIG_WITH_MEMORY: &str = r#"
[meta]
schema_version = 1
profile = "q35-ovmf-static-acpi"

[platform]
arch = "x86_64"
machine = "q35"

[cpu]
model = "host"

[cpu.topology]
cpus = 4
maxcpus = 4

[memory]
size = "2G"
backend = ""
memory_encryption = ""
aux_ram_share = false
"#;

    #[test]
    fn test_parse_args() {
        let args = vec![
            String::from("qemu-acpi"),
            String::from("config.toml"),
            String::from("-o"),
            String::from("out.bin"),
        ];
        let (input, output) = parse_args(&args).unwrap();
        assert_eq!(input, PathBuf::from("config.toml"));
        assert_eq!(output, PathBuf::from("out.bin"));
    }

    #[test]
    fn test_validate_config() {
        let config: Config = toml::from_str(SAMPLE_CONFIG).unwrap();
        let profile = validate_config(&config).unwrap();
        assert_eq!(
            profile,
            QemuQ35AcpiProfile {
                cpu_count: 4,
                max_cpu_count: 4,
            }
        );
    }

    #[test]
    fn test_memory_is_parsed_but_ignored() {
        let config: Config = toml::from_str(SAMPLE_CONFIG_WITH_MEMORY).unwrap();
        let profile = validate_config(&config).unwrap();
        assert_eq!(config.memory.size, "2G");
        assert_eq!(
            profile,
            QemuQ35AcpiProfile {
                cpu_count: 4,
                max_cpu_count: 4,
            }
        );
    }

    #[test]
    fn test_reject_acpi_override() {
        let config: Config = toml::from_str(
            r#"
[meta]
profile = "q35-ovmf-static-acpi"

[platform]
arch = "x86_64"
machine = "q35"

[cpu]

[cpu.topology]
cpus = 4
maxcpus = 4

[acpi]
tables = [{ kind = "file", signature = "DSDT", source = "base.bin" }]
"#,
        )
        .unwrap();
        assert!(matches!(
            validate_config(&config),
            Err(CliError::Unsupported(_))
        ));
    }
}
