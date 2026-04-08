// Copyright 2026
//
// SPDX-License-Identifier: Apache-2.0
//

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::fmt;

use zerocopy::{
    byteorder::{self, LE},
    FromBytes, Immutable, IntoBytes,
};

use crate::{
    aml::{
        Arg, BufferData, Device, EISAName, Method, MethodCall, Name, ResourceTemplate, Scope, IO,
    },
    facs::FACS,
    gas::{AccessSize, AddressSpace, GAS},
    sdt::Sdt,
    Aml, AmlSink,
};

type U16 = byteorder::U16<LE>;
type U32 = byteorder::U32<LE>;
type U64 = byteorder::U64<LE>;

const QEMU_OEM_ID: [u8; 6] = *b"BOCHS ";
const QEMU_OEM_TABLE_ID: [u8; 8] = *b"BXPC    ";
const QEMU_OEM_REVISION: u32 = 1;
const QEMU_CREATOR_ID: [u8; 4] = *b"BXPC";
const QEMU_CREATOR_REVISION: [u8; 4] = [1, 0, 0, 0];

const DSDT_OFFSET: usize = 0x0040;
const FADT_OFFSET: usize = 0x250e;
const DSDT_PROCESSOR_BLOCK_OFFSET: usize = 7405;
const DSDT_POST_PROCESSOR_BLOCK_OFFSET: usize = 8570;
const DSDT_GENERATED_TAIL_OFFSET: usize = 9192;

pub const QEMU_Q35_ACPI_BLOB_LEN: usize = 128 * 1024;
pub const QEMU_Q35_CPU_COUNT: u8 = 16;
pub const QEMU_Q35_MAX_CPU_COUNT: u8 = 16;

const QEMU_Q35_DSDT_PREFIX_HEX: &str = include_str!("qemu_q35_dsdt_prefix.hex");
const QEMU_Q35_DSDT_MID_HEX: &str = include_str!("qemu_q35_dsdt_mid.hex");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QemuQ35AcpiProfile {
    pub cpu_count: u8,
    pub max_cpu_count: u8,
}

impl Default for QemuQ35AcpiProfile {
    fn default() -> Self {
        Self {
            cpu_count: QEMU_Q35_CPU_COUNT,
            max_cpu_count: QEMU_Q35_MAX_CPU_COUNT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QemuAcpiError {
    ZeroCpus,
    CpuCountExceedsMax {
        cpu_count: u8,
        max_cpu_count: u8,
    },
    MaxCpuCountTooLarge {
        max_cpu_count: u8,
        supported_max: u8,
    },
}

impl fmt::Display for QemuAcpiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QemuAcpiError::ZeroCpus => write!(f, "cpu.topology.cpus must be greater than 0"),
            QemuAcpiError::CpuCountExceedsMax {
                cpu_count,
                max_cpu_count,
            } => write!(
                f,
                "cpu.topology.cpus ({cpu_count}) must be less than or equal to cpu.topology.maxcpus ({max_cpu_count})"
            ),
            QemuAcpiError::MaxCpuCountTooLarge {
                max_cpu_count,
                supported_max,
            } => write!(
                f,
                "cpu.topology.maxcpus ({max_cpu_count}) exceeds the current static q35 template limit ({supported_max})"
            ),
        }
    }
}

impl QemuQ35AcpiProfile {
    fn validate(self) -> Result<Self, QemuAcpiError> {
        if self.cpu_count == 0 {
            return Err(QemuAcpiError::ZeroCpus);
        }
        if self.cpu_count > self.max_cpu_count {
            return Err(QemuAcpiError::CpuCountExceedsMax {
                cpu_count: self.cpu_count,
                max_cpu_count: self.max_cpu_count,
            });
        }
        if self.max_cpu_count > QEMU_Q35_MAX_CPU_COUNT {
            return Err(QemuAcpiError::MaxCpuCountTooLarge {
                max_cpu_count: self.max_cpu_count,
                supported_max: QEMU_Q35_MAX_CPU_COUNT,
            });
        }
        Ok(self)
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, IntoBytes, Immutable, FromBytes)]
struct LocalApicEntry {
    entry_type: u8,
    length: u8,
    processor_uid: u8,
    apic_id: u8,
    flags: U32,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, IntoBytes, Immutable, FromBytes)]
struct IoApicEntry {
    entry_type: u8,
    length: u8,
    io_apic_id: u8,
    reserved: u8,
    io_apic_addr: U32,
    gsi_base: U32,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, IntoBytes, Immutable, FromBytes)]
struct InterruptSourceOverrideEntry {
    entry_type: u8,
    length: u8,
    bus: u8,
    source: u8,
    gsi: U32,
    flags: U16,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, IntoBytes, Immutable, FromBytes)]
struct LocalApicNmiEntry {
    entry_type: u8,
    length: u8,
    processor_uid: u8,
    flags: U16,
    lint: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default, IntoBytes, Immutable, FromBytes)]
struct EcamEntry {
    base_addr: U64,
    segment: U16,
    start_bus: u8,
    end_bus: u8,
    reserved: [u8; 4],
}

struct IrqNoFlags {
    irq: u8,
}

impl IrqNoFlags {
    fn new(irq: u8) -> Self {
        Self { irq }
    }
}

impl Aml for IrqNoFlags {
    fn to_aml_bytes(&self, sink: &mut dyn crate::AmlSink) {
        sink.byte(0x22);
        sink.word(1u16 << self.irq);
    }
}

struct ReturnOp<'a> {
    value: &'a dyn Aml,
}

impl<'a> ReturnOp<'a> {
    fn new(value: &'a dyn Aml) -> Self {
        Self { value }
    }
}

impl Aml for ReturnOp<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        sink.byte(0xa4);
        self.value.to_aml_bytes(sink);
    }
}

struct Processor<'a> {
    name: [u8; 4],
    proc_id: u8,
    children: Vec<&'a dyn Aml>,
}

impl<'a> Processor<'a> {
    fn new(name: [u8; 4], proc_id: u8, children: Vec<&'a dyn Aml>) -> Self {
        Self {
            name,
            proc_id,
            children,
        }
    }
}

fn pkg_length(len: usize, include_self: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(4);
    let length_length = if len < (2usize.pow(6) - 1) {
        1
    } else if len < (2usize.pow(12) - 2) {
        2
    } else if len < (2usize.pow(20) - 3) {
        3
    } else {
        4
    };
    let length = len + if include_self { length_length } else { 0 };
    match length_length {
        1 => result.push(length as u8),
        2 => {
            result.push((1u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8);
        }
        3 => {
            result.push((2u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8);
            result.push((length >> 12) as u8);
        }
        _ => {
            result.push((3u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8);
            result.push((length >> 12) as u8);
            result.push((length >> 20) as u8);
        }
    }
    result
}

impl Aml for Processor<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let mut body = Vec::new();
        body.extend_from_slice(&self.name);
        body.push(self.proc_id);
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(0);
        for child in &self.children {
            child.to_aml_bytes(&mut body);
        }

        sink.byte(0x5b);
        sink.byte(0x83);
        sink.vec(&pkg_length(body.len(), true));
        sink.vec(&body);
    }
}

fn decode_hex(hex: &str) -> Vec<u8> {
    fn nibble(ch: u8) -> u8 {
        match ch {
            b'0'..=b'9' => ch - b'0',
            b'a'..=b'f' => ch - b'a' + 10,
            b'A'..=b'F' => ch - b'A' + 10,
            _ => panic!("invalid hex digit"),
        }
    }

    let filtered: Vec<u8> = hex
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    assert_eq!(filtered.len() % 2, 0);

    let mut bytes = Vec::with_capacity(filtered.len() / 2);
    for pair in filtered.chunks_exact(2) {
        bytes.push((nibble(pair[0]) << 4) | nibble(pair[1]));
    }

    bytes
}

fn set_qemu_creator(sdt: &mut Sdt) {
    sdt.write_bytes(28, &QEMU_CREATOR_ID);
    sdt.write_bytes(32, &QEMU_CREATOR_REVISION);
}

fn append_aml(bytes: &mut Vec<u8>, aml: &dyn Aml) {
    aml.to_aml_bytes(bytes);
}

fn build_qemu_dsdt_generated_tail() -> Vec<u8> {
    let hid_kbd = Name::new("_HID".into(), &EISAName::new("PNP0303"));
    let sta_present = Name::new("_STA".into(), &0x0fu8);
    let kbd_crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(vec![
            &IO::new(0x60, 0x60, 1, 1),
            &IO::new(0x64, 0x64, 1, 1),
            &IrqNoFlags::new(1),
        ]),
    );
    let kbd = Device::new("KBD_".into(), vec![&hid_kbd, &sta_present, &kbd_crs]);

    let hid_mou = Name::new("_HID".into(), &EISAName::new("PNP0F13"));
    let mou_crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(vec![&IrqNoFlags::new(12)]),
    );
    let mou = Device::new("MOU_".into(), vec![&hid_mou, &sta_present, &mou_crs]);

    let hid_com1 = Name::new("_HID".into(), &EISAName::new("PNP0501"));
    let uid_one = Name::new("_UID".into(), &1u8);
    let com1_crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(vec![&IO::new(0x3f8, 0x3f8, 0, 8), &IrqNoFlags::new(4)]),
    );
    let com1 = Device::new(
        "COM1".into(),
        vec![&hid_com1, &uid_one, &sta_present, &com1_crs],
    );

    let hid_rtc = Name::new("_HID".into(), &EISAName::new("PNP0B00"));
    let rtc_crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(vec![&IO::new(0x70, 0x70, 1, 8), &IrqNoFlags::new(8)]),
    );
    let rtc = Device::new("RTC_".into(), vec![&hid_rtc, &rtc_crs]);

    let sfa_adr = Name::new("_ADR".into(), &0x001f_0002u32);
    let sfa = Device::new("SFA_".into(), vec![&sfa_adr]);
    let sfb_adr = Name::new("_ADR".into(), &0x001f_0003u32);
    let sfb = Device::new("SFB_".into(), vec![&sfb_adr]);

    let e01 = Method::new("_E01".into(), 0, false, vec![]);
    let gpe = Scope::new("_GPE".into(), vec![&e01]);

    let mut bytes = Vec::new();
    append_aml(&mut bytes, &kbd);
    append_aml(&mut bytes, &mou);
    append_aml(&mut bytes, &com1);
    append_aml(&mut bytes, &rtc);
    append_aml(&mut bytes, &sfa);
    append_aml(&mut bytes, &sfb);
    append_aml(&mut bytes, &gpe);
    bytes
}

fn cpu_name(uid: u8) -> [u8; 4] {
    let text = alloc::format!("C{uid:03X}");
    let mut out = [0u8; 4];
    out.copy_from_slice(text.as_bytes());
    out
}

fn cpu_mat(uid: u8) -> BufferData {
    BufferData::new(vec![0x00, 0x08, uid, uid, 0x01, 0x00, 0x00, 0x00])
}

fn build_qemu_dsdt_processor_block(max_cpu_count: u8) -> Vec<u8> {
    let mut bytes = Vec::new();

    for uid in 0..max_cpu_count {
        let uid_sta = uid;
        let sta_call = MethodCall::new("CSTA".into(), vec![&uid_sta]);
        let sta_ret = ReturnOp::new(&sta_call);
        let sta = Method::new("_STA".into(), 0, true, vec![&sta_ret]);

        let mat_buf = cpu_mat(uid);
        let mat = Name::new("_MAT".into(), &mat_buf);

        let uid_ej = uid;
        let ej0_call = MethodCall::new("CEJ0".into(), vec![&uid_ej]);
        let ej0 = (uid != 0).then(|| Method::new("_EJ0".into(), 1, false, vec![&ej0_call]));

        let uid_ost = uid;
        let cost_call = MethodCall::new("COST".into(), vec![&uid_ost, &Arg(0), &Arg(1), &Arg(2)]);
        let ost = Method::new("_OST".into(), 3, true, vec![&cost_call]);

        let mut children: Vec<&dyn Aml> = vec![&sta, &mat];
        if let Some(ej0) = &ej0 {
            children.push(ej0);
        }
        children.push(&ost);

        append_aml(&mut bytes, &Processor::new(cpu_name(uid), uid, children));
    }

    bytes
}

fn build_qemu_dsdt() -> Vec<u8> {
    let mut bytes = decode_hex(QEMU_Q35_DSDT_PREFIX_HEX);
    assert_eq!(bytes.len(), DSDT_PROCESSOR_BLOCK_OFFSET);
    bytes.extend_from_slice(&build_qemu_dsdt_processor_block(QEMU_Q35_MAX_CPU_COUNT));
    assert_eq!(bytes.len(), DSDT_POST_PROCESSOR_BLOCK_OFFSET);
    bytes.extend_from_slice(&decode_hex(QEMU_Q35_DSDT_MID_HEX));
    assert_eq!(bytes.len(), DSDT_GENERATED_TAIL_OFFSET);
    bytes.extend_from_slice(&build_qemu_dsdt_generated_tail());
    bytes
}

fn build_qemu_fadt() -> Sdt {
    let mut fadt = Sdt::new(
        *b"FACP",
        244,
        3,
        QEMU_OEM_ID,
        QEMU_OEM_TABLE_ID,
        QEMU_OEM_REVISION,
    );
    set_qemu_creator(&mut fadt);
    fadt.write_u32(36, 0);
    fadt.write_u32(40, DSDT_OFFSET as u32);
    fadt.write_u8(44, 1);
    fadt.write_u8(45, 0);
    fadt.write_u16(46, 9);
    fadt.write_u32(48, 0);
    fadt.write_u8(52, 0);
    fadt.write_u8(53, 0);
    fadt.write_u8(54, 0);
    fadt.write_u8(55, 0);
    fadt.write_u32(56, 0x600);
    fadt.write_u32(60, 0);
    fadt.write_u32(64, 0x604);
    fadt.write_u32(68, 0);
    fadt.write_u32(72, 0);
    fadt.write_u32(76, 0x608);
    fadt.write_u32(80, 0x620);
    fadt.write_u32(84, 0);
    fadt.write_u8(88, 4);
    fadt.write_u8(89, 2);
    fadt.write_u8(90, 0);
    fadt.write_u8(91, 4);
    fadt.write_u8(92, 16);
    fadt.write_u8(93, 0);
    fadt.write_u8(94, 0);
    fadt.write_u8(95, 0);
    fadt.write_u16(96, 0x0fff);
    fadt.write_u16(98, 0x0fff);
    fadt.write_u16(100, 0);
    fadt.write_u16(102, 0);
    fadt.write_u8(104, 0);
    fadt.write_u8(105, 0);
    fadt.write_u8(106, 0);
    fadt.write_u8(107, 0);
    fadt.write_u8(108, 50);
    fadt.write_u16(109, 0x0002);
    fadt.write_u8(111, 0);
    fadt.write_u32(112, 0x0004_84a5);
    fadt.write_bytes(
        116,
        GAS::new(AddressSpace::SystemIo, 8, 0, AccessSize::Undefined, 0x0cf9).as_bytes(),
    );
    fadt.write_u8(128, 0x0f);
    fadt.write_u16(129, 0);
    fadt.write_u8(131, 0);
    fadt.write_u64(132, 0);
    fadt.write_u64(140, DSDT_OFFSET as u64);
    fadt.write_bytes(
        148,
        GAS::new(AddressSpace::SystemIo, 32, 0, AccessSize::Undefined, 0x600).as_bytes(),
    );
    fadt.write_bytes(160, GAS::default().as_bytes());
    fadt.write_bytes(
        172,
        GAS::new(AddressSpace::SystemIo, 16, 0, AccessSize::Undefined, 0x604).as_bytes(),
    );
    fadt.write_bytes(184, GAS::default().as_bytes());
    fadt.write_bytes(196, GAS::default().as_bytes());
    fadt.write_bytes(
        208,
        GAS::new(AddressSpace::SystemIo, 32, 0, AccessSize::Undefined, 0x608).as_bytes(),
    );
    fadt.write_bytes(
        220,
        GAS::new(AddressSpace::SystemIo, 128, 0, AccessSize::Undefined, 0x620).as_bytes(),
    );
    fadt.write_bytes(232, GAS::default().as_bytes());
    fadt
}

fn build_qemu_madt(cpu_count: u8) -> Sdt {
    let mut madt = Sdt::new(
        *b"APIC",
        44,
        3,
        QEMU_OEM_ID,
        QEMU_OEM_TABLE_ID,
        QEMU_OEM_REVISION,
    );
    set_qemu_creator(&mut madt);
    madt.write_u32(36, 0xfee0_0000);
    madt.write_u32(40, 1);

    for cpu in 0..cpu_count {
        madt.append(LocalApicEntry {
            entry_type: 0,
            length: 8,
            processor_uid: cpu,
            apic_id: cpu,
            flags: 1.into(),
        });
    }

    madt.append(IoApicEntry {
        entry_type: 1,
        length: 12,
        io_apic_id: 0,
        reserved: 0,
        io_apic_addr: 0xfec0_0000.into(),
        gsi_base: 0.into(),
    });

    for irq in 0..cpu_count {
        madt.append(InterruptSourceOverrideEntry {
            entry_type: 2,
            length: 10,
            bus: 0,
            source: irq,
            gsi: u32::from(if irq == 0 { 2 } else { irq }).into(),
            flags: 5.into(),
        });
    }

    madt.append(LocalApicNmiEntry {
        entry_type: 4,
        length: 6,
        processor_uid: 0xff,
        flags: 0.into(),
        lint: 1,
    });
    madt
}

fn build_qemu_mcfg() -> Sdt {
    let mut mcfg = Sdt::new(
        *b"MCFG",
        44,
        1,
        QEMU_OEM_ID,
        QEMU_OEM_TABLE_ID,
        QEMU_OEM_REVISION,
    );
    set_qemu_creator(&mut mcfg);
    mcfg.append(EcamEntry {
        base_addr: 0xe000_0000.into(),
        segment: 0.into(),
        start_bus: 0,
        end_bus: 0xff,
        reserved: [0; 4],
    });
    mcfg
}

fn build_qemu_waet() -> Sdt {
    let mut waet = Sdt::new(
        *b"WAET",
        40,
        1,
        QEMU_OEM_ID,
        QEMU_OEM_TABLE_ID,
        QEMU_OEM_REVISION,
    );
    set_qemu_creator(&mut waet);
    waet.write_u32(36, 2);
    waet
}

fn build_qemu_rsdt(fadt: u32, madt: u32, mcfg: u32, waet: u32) -> Sdt {
    let mut rsdt = Sdt::new(
        *b"RSDT",
        52,
        1,
        QEMU_OEM_ID,
        QEMU_OEM_TABLE_ID,
        QEMU_OEM_REVISION,
    );
    set_qemu_creator(&mut rsdt);
    rsdt.write_u32(36, fadt);
    rsdt.write_u32(40, madt);
    rsdt.write_u32(44, mcfg);
    rsdt.write_u32(48, waet);
    rsdt
}

pub fn qemu_q35_acpi_table() -> Vec<u8> {
    qemu_q35_acpi_table_from_profile(QemuQ35AcpiProfile::default()).unwrap()
}

pub fn qemu_q35_acpi_table_from_profile(
    profile: QemuQ35AcpiProfile,
) -> Result<Vec<u8>, QemuAcpiError> {
    let profile = profile.validate()?;
    let dsdt = build_qemu_dsdt();
    let fadt = build_qemu_fadt();
    let madt = build_qemu_madt(profile.cpu_count);
    let mcfg = build_qemu_mcfg();
    let waet = build_qemu_waet();
    let mut facs = FACS::new();
    facs.version = 0;

    let madt_offset = FADT_OFFSET + fadt.len();
    let mcfg_offset = madt_offset + madt.len();
    let waet_offset = mcfg_offset + mcfg.len();
    let rsdt_offset = waet_offset + waet.len();
    let rsdt = build_qemu_rsdt(
        FADT_OFFSET as u32,
        madt_offset as u32,
        mcfg_offset as u32,
        waet_offset as u32,
    );

    let mut table = Vec::with_capacity(QEMU_Q35_ACPI_BLOB_LEN);
    facs.to_aml_bytes(&mut table);
    assert_eq!(table.len(), DSDT_OFFSET);

    table.extend_from_slice(&dsdt);
    assert_eq!(table.len(), FADT_OFFSET);

    table.extend_from_slice(fadt.as_slice());
    assert_eq!(table.len(), madt_offset);

    table.extend_from_slice(madt.as_slice());
    assert_eq!(table.len(), mcfg_offset);

    table.extend_from_slice(mcfg.as_slice());
    assert_eq!(table.len(), waet_offset);

    table.extend_from_slice(waet.as_slice());
    assert_eq!(table.len(), rsdt_offset);

    table.extend_from_slice(rsdt.as_slice());
    for offset in [
        FADT_OFFSET,
        madt_offset,
        mcfg_offset,
        waet_offset,
        rsdt_offset,
    ] {
        table[offset + 9] = 0;
    }
    table.resize(QEMU_Q35_ACPI_BLOB_LEN, 0);
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a64(data: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in data {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    #[test]
    fn test_qemu_q35_layout() {
        let blob = qemu_q35_acpi_table();
        let madt_offset = FADT_OFFSET + 244;
        let mcfg_offset = 0x2760;
        let waet_offset = 0x279c;
        let rsdt_offset = 0x27c4;

        assert_eq!(blob.len(), QEMU_Q35_ACPI_BLOB_LEN);
        assert_eq!(&blob[0..4], b"FACS");
        assert_eq!(&blob[DSDT_OFFSET..DSDT_OFFSET + 4], b"DSDT");
        assert_eq!(&blob[FADT_OFFSET..FADT_OFFSET + 4], b"FACP");
        assert_eq!(&blob[madt_offset..madt_offset + 4], b"APIC");
        assert_eq!(&blob[mcfg_offset..mcfg_offset + 4], b"MCFG");
        assert_eq!(&blob[waet_offset..waet_offset + 4], b"WAET");
        assert_eq!(&blob[rsdt_offset..rsdt_offset + 4], b"RSDT");
    }

    #[test]
    fn test_qemu_q35_exact_blob() {
        let blob = qemu_q35_acpi_table();
        assert_eq!(fnv1a64(&blob), 0x02c5_be1f_14f4_b9c4);
    }

    #[test]
    fn test_qemu_q35_dsdt_generated_tail_size() {
        let tail = build_qemu_dsdt_generated_tail();
        assert_eq!(tail.len(), 230);
        let dsdt = build_qemu_dsdt();
        assert_eq!(dsdt.len(), 9422);
        assert_eq!(&dsdt[DSDT_GENERATED_TAIL_OFFSET..], &tail);
    }

    #[test]
    fn test_qemu_q35_dsdt_processor_block_size() {
        let block = build_qemu_dsdt_processor_block(QEMU_Q35_MAX_CPU_COUNT);
        assert_eq!(
            block.len(),
            DSDT_POST_PROCESSOR_BLOCK_OFFSET - DSDT_PROCESSOR_BLOCK_OFFSET
        );
    }

    #[test]
    fn test_qemu_q35_custom_cpu_count() {
        let blob = qemu_q35_acpi_table_from_profile(QemuQ35AcpiProfile {
            cpu_count: 4,
            max_cpu_count: 4,
        })
        .unwrap();
        assert_eq!(blob.len(), QEMU_Q35_ACPI_BLOB_LEN);
        assert_eq!(&blob[FADT_OFFSET + 244..FADT_OFFSET + 248], b"APIC");
    }

    #[test]
    fn test_qemu_q35_rejects_invalid_profile() {
        assert_eq!(
            qemu_q35_acpi_table_from_profile(QemuQ35AcpiProfile {
                cpu_count: 0,
                max_cpu_count: 4,
            }),
            Err(QemuAcpiError::ZeroCpus)
        );
        assert_eq!(
            qemu_q35_acpi_table_from_profile(QemuQ35AcpiProfile {
                cpu_count: 5,
                max_cpu_count: 4,
            }),
            Err(QemuAcpiError::CpuCountExceedsMax {
                cpu_count: 5,
                max_cpu_count: 4,
            })
        );
    }
}
