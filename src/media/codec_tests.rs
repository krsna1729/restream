use super::aac::{has_adts_sync, prepend_adts};
use super::*;
use crate::media::packet::PayloadFormat;
use proptest::prelude::*;
use std::borrow::Cow;

fn push_bits(bits: &mut Vec<bool>, value: u64, width: usize) {
    for shift in (0..width).rev() {
        bits.push(((value >> shift) & 1) == 1);
    }
}

fn push_ue(bits: &mut Vec<bool>, value: u64) {
    let code_num = value + 1;
    let width = 64 - code_num.leading_zeros() as usize;
    bits.extend(std::iter::repeat_n(false, width.saturating_sub(1)));
    push_bits(bits, code_num, width);
}

fn pack_bits(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            chunk.iter().enumerate().fold(0u8, |byte, (index, bit)| {
                byte | (u8::from(*bit) << (7 - index))
            })
        })
        .collect()
}

fn insert_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zero_run = 0u8;
    for &byte in data {
        if zero_run >= 2 && byte <= 3 {
            out.push(3);
            zero_run = 0;
        }
        out.push(byte);
        zero_run = if byte == 0 {
            zero_run.saturating_add(1)
        } else {
            0
        };
    }
    out
}

fn minimal_hevc_sps_nalu(chroma_format_idc: u64, bit_depth_minus8: u64) -> Vec<u8> {
    let mut bits = Vec::new();
    push_bits(&mut bits, 0, 4);
    push_bits(&mut bits, 0, 3);
    push_bits(&mut bits, 1, 1);
    push_bits(&mut bits, 0, 2);
    push_bits(&mut bits, 0, 1);
    push_bits(&mut bits, 2, 5);
    push_bits(&mut bits, 0, 32);
    push_bits(&mut bits, 0, 48);
    push_bits(&mut bits, 0x7b, 8);
    push_ue(&mut bits, 0);
    push_ue(&mut bits, chroma_format_idc);
    push_ue(&mut bits, 1920);
    push_ue(&mut bits, 1080);
    push_bits(&mut bits, 0, 1);
    push_ue(&mut bits, bit_depth_minus8);
    push_ue(&mut bits, bit_depth_minus8);

    let mut sps = vec![0x42, 0x01];
    sps.extend(insert_emulation_prevention(&pack_bits(&bits)));
    sps
}

include!("codec_tests/format_conversion.rs");
include!("codec_tests/annexb_avcc.rs");
include!("codec_tests/transport_stream.rs");
