//! Encode GPUI's straight-alpha BGRA rasters into native assets in Rust.
use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context as _, Result, ensure};

use crate::{cargo, execute};

const SIZES: [u32; 8] = [16, 32, 48, 64, 128, 256, 512, 1024];
const WINDOWS_SIZES: [u32; 6] = [16, 32, 48, 64, 128, 256];
const ICNS_SLOTS: [([u8; 4], u32); 11] = [
    (*b"icp4", 16),
    (*b"icp5", 32),
    (*b"icp6", 64),
    (*b"ic07", 128),
    (*b"ic08", 256),
    (*b"ic09", 512),
    (*b"ic10", 1024),
    (*b"ic11", 32),
    (*b"ic12", 64),
    (*b"ic13", 256),
    (*b"ic14", 512),
];

fn png(size: u32, bgra: &[u8]) -> Result<Vec<u8>> {
    let expected = usize::try_from(size)?
        .checked_mul(usize::try_from(size)?)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("raster size overflow")?;
    ensure!(
        size > 0 && bgra.len() == expected,
        "unexpected raster size for {size}px"
    );
    let mut rgba = bgra.to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba)?;
        writer.finish()?;
    }
    Ok(bytes)
}

fn frame(frames: &BTreeMap<u32, Vec<u8>>, size: u32) -> Result<&[u8]> {
    frames
        .get(&size)
        .map(Vec::as_slice)
        .with_context(|| format!("missing {size}px icon frame"))
}

fn icns(frames: &BTreeMap<u32, Vec<u8>>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for (kind, size) in ICNS_SLOTS {
        let image = frame(frames, size)?;
        body.extend_from_slice(&kind);
        body.extend_from_slice(&u32::try_from(image.len() + 8)?.to_be_bytes());
        body.extend_from_slice(image);
    }
    let mut bytes = b"icns".to_vec();
    bytes.extend_from_slice(&u32::try_from(body.len() + 8)?.to_be_bytes());
    bytes.extend(body);
    Ok(bytes)
}

fn ico(frames: &BTreeMap<u32, Vec<u8>>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(WINDOWS_SIZES.len())?.to_le_bytes());
    let mut offset = u32::try_from(6 + WINDOWS_SIZES.len() * 16)?;
    for size in WINDOWS_SIZES {
        let length = u32::try_from(frame(frames, size)?.len())?;
        bytes.extend_from_slice(&[(size % 256) as u8, (size % 256) as u8, 0, 0]);
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&32_u16.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        offset = offset.checked_add(length).context("ICO size overflow")?;
    }
    for size in WINDOWS_SIZES {
        bytes.extend_from_slice(frame(frames, size)?);
    }
    Ok(bytes)
}

pub(crate) fn generate(root: &Path, destination: &Path) -> Result<()> {
    execute(cargo(root).args([
        "run",
        "-p",
        "tgbooks-app",
        "--features",
        "gpui-tests",
        "--locked",
        "--",
        "--render-app-icon",
    ]))?;
    encode_rasters(&root.join("target/icon-raster"), destination)
}

fn encode_rasters(rasters: &Path, destination: &Path) -> Result<()> {
    let mut frames = BTreeMap::new();
    for size in SIZES {
        frames.insert(
            size,
            png(size, &fs::read(rasters.join(format!("{size}.bgra")))?)?,
        );
    }
    // Validate every frame and encode both containers before replacing any asset.
    let icns = icns(&frames)?;
    let ico = ico(&frames)?;
    fs::create_dir_all(destination)?;
    fs::write(destination.join("app-icon.png"), frame(&frames, 1024)?)?;
    fs::write(destination.join("app-icon.icns"), icns)?;
    fs::write(destination.join("app-icon.ico"), ico)?;
    println!("Generated PNG, ICNS and ICO in {}", destination.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
        let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info()?;
        let mut pixels = vec![
            0;
            reader
                .output_buffer_size()
                .context("missing PNG buffer size")?
        ];
        let info = reader.next_frame(&mut pixels)?;
        pixels.truncate(info.buffer_size());
        Ok(pixels)
    }

    #[test]
    fn png_swaps_channels_without_losing_alpha_and_rejects_wrong_sizes() -> Result<()> {
        assert_eq!(decode(&png(1, &[12, 34, 56, 78])?)?, [56, 34, 12, 78]);
        assert_eq!(decode(&png(1, &[0, 0, 0, 0])?)?, [0, 0, 0, 0]);
        assert!(png(0, &[]).is_err());
        assert!(png(32, &[0; 4]).is_err());
        Ok(())
    }

    #[test]
    fn native_containers_have_correct_slots_offsets_and_png_frames() -> Result<()> {
        let frames = SIZES
            .into_iter()
            .map(|size| Ok((size, png(size, &vec![255; (size * size * 4) as usize])?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let bytes = icns(&frames)?;
        assert_eq!(&bytes[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into()?) as usize,
            bytes.len()
        );
        let mut offset = 8;
        for (kind, size) in ICNS_SLOTS {
            assert_eq!(&bytes[offset..offset + 4], kind.as_slice());
            let length = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
            assert_eq!(&bytes[offset + 8..offset + length], frame(&frames, size)?);
            offset += length;
        }
        assert_eq!(offset, bytes.len());
        let bytes = ico(&frames)?;
        assert_eq!(&bytes[..6], &[0, 0, 1, 0, 6, 0]);
        for (index, size) in WINDOWS_SIZES.into_iter().enumerate() {
            let entry = 6 + index * 16;
            let length = u32::from_le_bytes(bytes[entry + 8..entry + 12].try_into()?) as usize;
            let offset = u32::from_le_bytes(bytes[entry + 12..entry + 16].try_into()?) as usize;
            assert_eq!(bytes[entry], (size % 256) as u8);
            assert_eq!(&bytes[offset..offset + length], frame(&frames, size)?);
        }
        assert!(icns(&BTreeMap::new()).is_err());
        assert!(ico(&BTreeMap::new()).is_err());
        Ok(())
    }

    #[test]
    fn invalid_rasters_do_not_replace_existing_assets() -> Result<()> {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("assets");
        fs::create_dir(&destination)?;
        fs::write(destination.join("app-icon.png"), b"keep")?;
        assert!(encode_rasters(root.path(), &destination).is_err());
        assert_eq!(fs::read(destination.join("app-icon.png"))?, b"keep");
        Ok(())
    }
}
