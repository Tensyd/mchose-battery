//! Embeds the shell icon into the executable.
//!
//! The window class icon covers the window and the tray, but Explorer, the taskbar and
//! Task Manager read the icon resource of the file itself. Rather than require a resource
//! compiler (`windres`/`rc.exe`, neither of which is guaranteed on a build machine), this
//! script writes the `.rsrc` COFF object itself: a resource directory holding the icon
//! group and its images. The linker then only has to place one read-only section.
//!
//! A missing or unreadable `assets/app.ico` is not fatal; the binary simply keeps the
//! generic icon.

use std::path::PathBuf;
use std::{env, fs};

const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const LANGUAGE: u32 = 0x0409;

const DIRECTORY_SIZE: u32 = 16;
const ENTRY_SIZE: u32 = 8;
const DATA_ENTRY_SIZE: u32 = 16;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let icon_path = manifest_dir.join("assets").join("app.ico");

    let Ok(icon_bytes) = fs::read(&icon_path) else {
        println!(
            "cargo:warning={} is missing; using the generic executable icon",
            icon_path.display()
        );
        return;
    };

    let images = match parse_ico(&icon_bytes) {
        Ok(images) if !images.is_empty() => images,
        Ok(_) => {
            println!("cargo:warning=assets/app.ico has no images; using the generic icon");
            return;
        }
        Err(error) => {
            println!("cargo:warning=could not read assets/app.ico ({error}); using the generic icon");
            return;
        }
    };

    let (section, rva_fields) = build_resource_section(&images);
    let object_path = out_dir.join("app-icon.o");

    if let Err(error) = fs::write(&object_path, wrap_in_coff(&section, &rva_fields)) {
        println!("cargo:warning=could not write the icon object ({error}); using the generic icon");
        return;
    }

    // A single section, not a library, so it goes straight to the linker.
    println!("cargo:rustc-link-arg={}", object_path.display());
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
/// data entry stores an image *RVA*, which is only known after linking. Those six fields
/// are therefore reported back so the object can carry relocations for them.
fn build_resource_section(images: &[IconImage]) -> (Vec<u8>, Vec<u32>) {
    let icon_count = images.len() as u32;
    let group = group_icon(images);

    // Directory sizes, in the order they are written.
    let icon_type_dir_size = DIRECTORY_SIZE + icon_count * ENTRY_SIZE;
    let name_dir_size = DIRECTORY_SIZE + ENTRY_SIZE;
    let group_type_dir_size = DIRECTORY_SIZE + ENTRY_SIZE;

    let icon_type_dir = DIRECTORY_SIZE + 2 * ENTRY_SIZE;
    let first_icon_name_dir = icon_type_dir + icon_type_dir_size;
    let group_type_dir = first_icon_name_dir + icon_count * name_dir_size;
    let group_name_dir = group_type_dir + group_type_dir_size;
    let data_entries = group_name_dir + name_dir_size;
    let payload_start = data_entries + DATA_ENTRY_SIZE * (icon_count + 1);

    // Payload offsets, four-byte aligned as the loader expects.
    let mut cursor = payload_start;
    let mut icon_payloads = Vec::with_capacity(images.len());
    for image in images {
        let offset = align4(cursor);
        icon_payloads.push(offset);
        cursor = offset + image.data.len() as u32;
    }
    let group_payload = align4(cursor);

    let mut blob = Vec::new();

    // Root: the two resource types.
    push_directory(&mut blob, 0, 2);
    push_entry(&mut blob, RT_ICON, icon_type_dir | 0x8000_0000);
    push_entry(&mut blob, RT_GROUP_ICON, group_type_dir | 0x8000_0000);

    // RT_ICON: one named entry per image.
    push_directory(&mut blob, 0, icon_count as u16);
    for id in 0..icon_count {
        let name_dir = first_icon_name_dir + id * name_dir_size;
        push_entry(&mut blob, id + 1, name_dir | 0x8000_0000);
    }

    // Each image has a single language directory pointing at its data entry.
    for index in 0..icon_count {
        push_directory(&mut blob, 0, 1);
        push_entry(
            &mut blob,
            LANGUAGE,
            data_entries + index * DATA_ENTRY_SIZE,
        );
    }

    // RT_GROUP_ICON: one named entry.
    push_directory(&mut blob, 0, 1);
    push_entry(&mut blob, 1, group_name_dir | 0x8000_0000);
    push_directory(&mut blob, 0, 1);
    push_entry(
        &mut blob,
        LANGUAGE,
        data_entries + icon_count * DATA_ENTRY_SIZE,
    );

    debug_assert_eq!(blob.len() as u32, data_entries);

    let mut rva_fields = Vec::with_capacity(images.len() + 1);
    for (image, offset) in images.iter().zip(icon_payloads.iter()) {
        rva_fields.push(blob.len() as u32);
        push_data_entry(&mut blob, *offset, image.data.len() as u32);
    }
    rva_fields.push(blob.len() as u32);
    push_data_entry(&mut blob, group_payload, group.len() as u32);

    debug_assert_eq!(blob.len() as u32, payload_start);

    for (image, offset) in images.iter().zip(icon_payloads.iter()) {
        pad_to(&mut blob, *offset);
        blob.extend_from_slice(&image.data);
    }
    pad_to(&mut blob, group_payload);
    blob.extend_from_slice(&group);

    (blob, rva_fields)
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
        group.extend_from_slice(&((index as u32) + 1).to_le_bytes()); // RT_ICON id
    }
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
