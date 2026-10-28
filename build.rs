//! Embeds the shell icon and the version resource into the executable.
//!
//! The window class icon covers the window and the tray, but Explorer, the taskbar and
//! Task Manager read the icon resource of the file itself. The version resource is what
//! File > Properties shows, and what a bug report can be checked against.
//!
//! Rather than require a resource compiler (`windres`/`rc.exe`, neither of which is
//! guaranteed on a build machine), this script writes the `.rsrc` COFF object itself: a
//! resource directory holding the icon group, its images and the version block. The
//! linker then only has to place one read-only section.
//!
//! A missing or unreadable `assets/app.ico` is not fatal; the binary simply keeps the
//! generic icon and still carries its version.

use std::path::PathBuf;
use std::{env, fs};

const RT_VERSION: u32 = 16;
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const LANGUAGE: u32 = 0x0409; // en-US, what the strings below are written in
const CODE_PAGE: u32 = 0x04B0; // Unicode BMP

const DIRECTORY_SIZE: u32 = 16;
const ENTRY_SIZE: u32 = 8;
const DATA_ENTRY_SIZE: u32 = 16;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=build.rs");
    // The embedded version string comes from CARGO_PKG_VERSION, so a version bump has to
    // re-run this script. Without this line the binary would quietly keep the old number.
    println!("cargo:rerun-if-changed=Cargo.toml");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let icon_path = manifest_dir.join("assets").join("app.ico");

    let images = fs::read(&icon_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| parse_ico(&bytes))
        .unwrap_or_else(|error| {
            println!(
                "cargo:warning=could not read {} ({error}); the binary will keep the generic icon",
                icon_path.display()
            );
            Vec::new()
        });

    let (section, rva_fields) = build_resource_section(&images);
    let object_path = out_dir.join("app-icon.o");

    if let Err(error) = fs::write(&object_path, wrap_in_coff(&section, &rva_fields)) {
        println!("cargo:warning=could not write the resource object ({error})");
        return;
    }

    // A single section, not a library, so it goes straight to the linker.
    println!("cargo:rustc-link-arg={}", object_path.display());
}

/// One leaf of the resource tree: a type, an id and its bytes.
struct Resource {
    type_id: u32,
    name_id: u32,
    data: Vec<u8>,
}

struct IconImage {
    width: u8,
    height: u8,
    color_count: u8,
    planes: u16,
    bit_count: u16,
    data: Vec<u8>,
}

/// Reads the directory of a `.ico` file and returns its images in declared order.
fn parse_ico(bytes: &[u8]) -> Result<Vec<IconImage>, String> {
    if bytes.len() < 6 {
        return Err("file is too short".to_string());
    }
    let reserved = u16::from_le_bytes([bytes[0], bytes[1]]);
    let kind = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if reserved != 0 || kind != 1 {
        return Err("not an icon file".to_string());
    }

    let mut images = Vec::with_capacity(count);
    for index in 0..count {
        let entry = 6 + index * 16;
        if entry + 16 > bytes.len() {
            return Err("truncated directory".to_string());
        }

        let size = read_u32(bytes, entry + 8) as usize;
        let offset = read_u32(bytes, entry + 12) as usize;
        let end = offset.checked_add(size).ok_or("bad image offset")?;
        if end > bytes.len() {
            return Err("image data runs past the file".to_string());
        }

        images.push(IconImage {
            width: bytes[entry],
            height: bytes[entry + 1],
            color_count: bytes[entry + 2],
            planes: u16::from_le_bytes([bytes[entry + 4], bytes[entry + 5]]),
            bit_count: u16::from_le_bytes([bytes[entry + 6], bytes[entry + 7]]),
            data: bytes[offset..end].to_vec(),
        });
    }
    Ok(images)
}

/// Lays out the resource directory tree and its payload.
///
/// Directory offsets inside a `.rsrc` section are relative to the section start, but each
/// data entry stores an image *RVA*, which is only known after linking. Those fields are
/// therefore reported back so the object can carry relocations for them.
fn build_resource_section(images: &[IconImage]) -> (Vec<u8>, Vec<u32>) {
    let mut resources: Vec<Resource> = Vec::new();
    for (index, image) in images.iter().enumerate() {
        resources.push(Resource {
            type_id: RT_ICON,
            name_id: index as u32 + 1,
            data: image.data.clone(),
        });
    }
    if !images.is_empty() {
        resources.push(Resource {
            type_id: RT_GROUP_ICON,
            name_id: 1,
            data: group_icon(images),
        });
    }
    resources.push(Resource {
        type_id: RT_VERSION,
        name_id: 1,
        data: version_info(),
    });

    // Types in first-seen order, each holding the ids that belong to it.
    let mut groups: Vec<(u32, Vec<usize>)> = Vec::new();
    for (index, resource) in resources.iter().enumerate() {
        match groups.iter_mut().find(|(id, _)| *id == resource.type_id) {
            Some((_, members)) => members.push(index),
            None => groups.push((resource.type_id, vec![index])),
        }
    }

    // Layout, in write order: root directory, one directory per type, one directory per
    // leaf (the language level), the data entries, then the payloads.
    let mut offset = DIRECTORY_SIZE + ENTRY_SIZE * groups.len() as u32;

    let mut type_dir_offsets = Vec::with_capacity(groups.len());
    for (_, members) in &groups {
        type_dir_offsets.push(offset);
        offset += DIRECTORY_SIZE + ENTRY_SIZE * members.len() as u32;
    }

    let mut name_dir_offsets = Vec::with_capacity(resources.len());
    for _ in &resources {
        name_dir_offsets.push(offset);
        offset += DIRECTORY_SIZE + ENTRY_SIZE; // exactly one language
    }

    let data_entries = offset;
    let payload_start = data_entries + DATA_ENTRY_SIZE * resources.len() as u32;

    let mut cursor = payload_start;
    let mut payload_offsets = Vec::with_capacity(resources.len());
    for resource in &resources {
        let aligned = align4(cursor);
        payload_offsets.push(aligned);
        cursor = aligned + resource.data.len() as u32;
    }

    let mut blob = Vec::new();

    // Root directory: one entry per resource type.
    push_directory(&mut blob, 0, groups.len() as u16);
    for ((type_id, _), type_dir) in groups.iter().zip(type_dir_offsets.iter()) {
        push_entry(&mut blob, *type_id, type_dir | 0x8000_0000);
    }

    // Each type directory, followed by the language directories of its leaves.
    for ((_, members), type_dir) in groups.iter().zip(type_dir_offsets.iter()) {
        debug_assert_eq!(blob.len() as u32, *type_dir);
        push_directory(&mut blob, 0, members.len() as u16);
        for member in members {
            push_entry(
                &mut blob,
                resources[*member].name_id,
                name_dir_offsets[*member] | 0x8000_0000,
            );
        }
    }

    for (index, name_dir) in name_dir_offsets.iter().enumerate() {
        debug_assert_eq!(blob.len() as u32, *name_dir);
        push_directory(&mut blob, 0, 1);
        push_entry(&mut blob, LANGUAGE, data_entries + index as u32 * DATA_ENTRY_SIZE);
    }

    debug_assert_eq!(blob.len() as u32, data_entries);

    let mut rva_fields = Vec::with_capacity(resources.len());
    for (resource, payload) in resources.iter().zip(payload_offsets.iter()) {
        rva_fields.push(blob.len() as u32);
        push_data_entry(&mut blob, *payload, resource.data.len() as u32);
    }

    debug_assert_eq!(blob.len() as u32, payload_start);

    for (resource, payload) in resources.iter().zip(payload_offsets.iter()) {
        pad_to(&mut blob, *payload);
        blob.extend_from_slice(&resource.data);
    }

    (blob, rva_fields)
}

/// RT_VERSION payload: the strings File > Properties shows.
///
/// Layout is the documented VS_VERSIONINFO tree: a fixed 52-byte block, then
/// StringFileInfo (one table of key/value pairs) and VarFileInfo (the language/code-page
/// pair the table is keyed by). Every block carries its own byte length and is padded to a
/// DWORD boundary, so lengths are patched in once the children are known.
fn version_info() -> Vec<u8> {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let mut parts = version.split('.').map(|part| part.parse::<u16>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let build = parts.next().unwrap_or(0);
    let version_text = format!("{major}.{minor}.{patch}.{build}");

    let fixed = {
        let mut value = Vec::with_capacity(52);
        value.extend_from_slice(&0xFEEF_04BD_u32.to_le_bytes()); // signature
        value.extend_from_slice(&0x0001_0000_u32.to_le_bytes()); // struct version
        value.extend_from_slice(&(u32::from(major) << 16 | u32::from(minor)).to_le_bytes());
        value.extend_from_slice(&(u32::from(patch) << 16 | u32::from(build)).to_le_bytes());
        value.extend_from_slice(&(u32::from(major) << 16 | u32::from(minor)).to_le_bytes());
        value.extend_from_slice(&(u32::from(patch) << 16 | u32::from(build)).to_le_bytes());
        value.extend_from_slice(&0x3F_u32.to_le_bytes()); // flags mask
        value.extend_from_slice(&0_u32.to_le_bytes()); // flags: no debug/prerelease
        value.extend_from_slice(&0x0004_0004_u32.to_le_bytes()); // VOS_NT_WINDOWS32
        value.extend_from_slice(&0x0000_0001_u32.to_le_bytes()); // VFT_APP
        value.extend_from_slice(&0_u32.to_le_bytes()); // subtype
        value.extend_from_slice(&0_u32.to_le_bytes()); // date MS
        value.extend_from_slice(&0_u32.to_le_bytes()); // date LS
        value
    };
    debug_assert_eq!(fixed.len(), 52);

    let strings = [
        ("CompanyName", "Tensyd".to_string()),
        ("FileDescription", "MC Battery 无线外设电量".to_string()),
        ("FileVersion", version_text.clone()),
        ("InternalName", "mc-battery".to_string()),
        ("LegalCopyright", "MIT License".to_string()),
        ("OriginalFilename", "MC-Battery.exe".to_string()),
        ("ProductName", "MC Battery".to_string()),
        ("ProductVersion", version_text),
    ];

    let mut table_children = Vec::new();
    for (key, value) in &strings {
        let words = value.encode_utf16().count() as u16 + 1; // plus the terminator
        table_children.extend(block(key, &wide_bytes(value), words, 1, &[]));
    }

    let table = block(
        &format!("{LANGUAGE:04X}{CODE_PAGE:04X}"),
        &[],
        0,
        1,
        &table_children,
    );
    let string_file_info = block("StringFileInfo", &[], 0, 1, &table);

    let translation = ((LANGUAGE << 16) | CODE_PAGE).to_le_bytes();
    let var = block("Translation", &translation, 4, 0, &[]);
    let var_file_info = block("VarFileInfo", &[], 0, 1, &var);

    let mut children = string_file_info;
    children.extend_from_slice(&var_file_info);

    block("VS_VERSION_INFO", &fixed, 52, 0, &children)
}

/// One VS_VERSIONINFO block: length, value length, type, key, value, children.
fn block(key: &str, value: &[u8], value_length: u16, kind: u16, children: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&0_u16.to_le_bytes()); // wLength, patched below
    blob.extend_from_slice(&value_length.to_le_bytes());
    blob.extend_from_slice(&kind.to_le_bytes());
    blob.extend_from_slice(&wide_bytes(key));
    pad4(&mut blob);
    if !value.is_empty() {
        blob.extend_from_slice(value);
        pad4(&mut blob);
    }
    blob.extend_from_slice(children);

    let length = blob.len() as u16;
    blob[0..2].copy_from_slice(&length.to_le_bytes());
    blob
}

fn wide_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

fn pad4(blob: &mut Vec<u8>) {
    while !blob.len().is_multiple_of(4) {
        blob.push(0);
    }
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn align4(value: u32) -> u32 {
    (value + 3) & !3
}

fn pad_to(blob: &mut Vec<u8>, offset: u32) {
    while (blob.len() as u32) < offset {
        blob.push(0);
    }
}

fn push_directory(blob: &mut Vec<u8>, named: u16, ids: u16) {
    blob.extend_from_slice(&0_u32.to_le_bytes()); // characteristics
    blob.extend_from_slice(&0_u32.to_le_bytes()); // timestamp
    blob.extend_from_slice(&0_u16.to_le_bytes()); // major version
    blob.extend_from_slice(&0_u16.to_le_bytes()); // minor version
    blob.extend_from_slice(&named.to_le_bytes());
    blob.extend_from_slice(&ids.to_le_bytes());
}

fn push_entry(blob: &mut Vec<u8>, id: u32, target: u32) {
    blob.extend_from_slice(&id.to_le_bytes());
    blob.extend_from_slice(&target.to_le_bytes());
}

fn push_data_entry(blob: &mut Vec<u8>, offset: u32, size: u32) {
    blob.extend_from_slice(&offset.to_le_bytes());
    blob.extend_from_slice(&size.to_le_bytes());
    blob.extend_from_slice(&0_u32.to_le_bytes()); // code page
    blob.extend_from_slice(&0_u32.to_le_bytes()); // reserved
}

/// GRPICONDIR: the icon group Explorer shows, referencing the images by id.
///
/// Entry layout is 14 bytes: bWidth, bHeight, bColorCount, bReserved, wPlanes, wBitCount,
/// dwBytesInRes, wID. The id is a WORD — writing it as a DWORD shifts every following
/// entry, which leaves the shell reading one valid entry and garbage after it.
fn group_icon(images: &[IconImage]) -> Vec<u8> {
    let mut group = Vec::new();
    group.extend_from_slice(&0_u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1_u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());

    for (index, image) in images.iter().enumerate() {
        group.push(image.width);
        group.push(image.height);
        group.push(image.color_count);
        group.push(0); // reserved
        group.extend_from_slice(&image.planes.to_le_bytes());
        group.extend_from_slice(&image.bit_count.to_le_bytes());
        group.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        group.extend_from_slice(&((index as u16) + 1).to_le_bytes()); // RT_ICON id
    }

    debug_assert_eq!(group.len(), 6 + 14 * images.len());
    group
}

/// Minimal COFF object with one read-only `.rsrc` section.
///
/// `rva_fields` lists the section-relative positions of the data-entry `OffsetToData`
/// words. Those must end up as RVAs, which only the linker knows, so each gets an
/// `IMAGE_REL_AMD64_ADDR32NB` relocation against the section symbol.
fn wrap_in_coff(section: &[u8], rva_fields: &[u32]) -> Vec<u8> {
    const MACHINE_AMD64: u16 = 0x8664;
    const FILE_HEADER_SIZE: u32 = 20;
    const SECTION_HEADER_SIZE: u32 = 40;
    const RELOCATION_SIZE: u32 = 10;
    const SYMBOL_SIZE: u32 = 18;
    const REL_AMD64_ADDR32NB: u16 = 0x0003;

    let raw_size = align4(section.len() as u32);
    let relocation_count = rva_fields.len() as u16;
    let relocations_offset = FILE_HEADER_SIZE + SECTION_HEADER_SIZE + raw_size;
    let symbols_offset = relocations_offset + RELOCATION_SIZE * rva_fields.len() as u32;

    let mut object = Vec::with_capacity(
        (symbols_offset + SYMBOL_SIZE + 4) as usize + section.len(),
    );

    object.extend_from_slice(&MACHINE_AMD64.to_le_bytes());
    object.extend_from_slice(&1_u16.to_le_bytes()); // number of sections
    object.extend_from_slice(&0_u32.to_le_bytes()); // timestamp
    object.extend_from_slice(&symbols_offset.to_le_bytes()); // symbol table
    object.extend_from_slice(&1_u32.to_le_bytes()); // number of symbols
    object.extend_from_slice(&0_u16.to_le_bytes()); // optional header size
    object.extend_from_slice(&0_u16.to_le_bytes()); // characteristics

    let mut name = [0_u8; 8];
    name[..5].copy_from_slice(b".rsrc");
    object.extend_from_slice(&name);
    object.extend_from_slice(&(section.len() as u32).to_le_bytes()); // virtual size
    object.extend_from_slice(&0_u32.to_le_bytes()); // virtual address
    object.extend_from_slice(&raw_size.to_le_bytes()); // raw size
    object.extend_from_slice(&(FILE_HEADER_SIZE + SECTION_HEADER_SIZE).to_le_bytes());
    object.extend_from_slice(&relocations_offset.to_le_bytes());
    object.extend_from_slice(&0_u32.to_le_bytes()); // line numbers
    object.extend_from_slice(&relocation_count.to_le_bytes());
    object.extend_from_slice(&0_u16.to_le_bytes()); // line number count
    // CNT_INITIALIZED_DATA | MEM_READ
    object.extend_from_slice(&0x4000_0040_u32.to_le_bytes());

    object.extend_from_slice(section);
    while object.len() < (FILE_HEADER_SIZE + SECTION_HEADER_SIZE + raw_size) as usize {
        object.push(0);
    }

    for field in rva_fields {
        object.extend_from_slice(&field.to_le_bytes()); // virtual address
        object.extend_from_slice(&0_u32.to_le_bytes()); // symbol index
        object.extend_from_slice(&REL_AMD64_ADDR32NB.to_le_bytes());
    }

    // One section symbol: value 0, section 1, static storage class.
    let mut symbol_name = [0_u8; 8];
    symbol_name[..5].copy_from_slice(b".rsrc");
    object.extend_from_slice(&symbol_name);
    object.extend_from_slice(&0_u32.to_le_bytes()); // value
    object.extend_from_slice(&1_i16.to_le_bytes()); // section number
    object.extend_from_slice(&0_u16.to_le_bytes()); // type
    object.push(3); // storage class: static
    object.push(0); // auxiliary symbols

    object.extend_from_slice(&4_u32.to_le_bytes()); // empty string table

    object
}
