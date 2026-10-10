//! Feature tests for `DataView` (ES 25.3): a byte-addressable view over an
//! `ArrayBuffer`, with explicit little-/big-endian control at read/write time.

use crate::helpers::{eval_bool, eval_number, eval_string};

#[test]
fn basic_read_write_with_endianness() {
    // Little-endian write, little-endian read back.
    assert_eq!(
        eval_string(
            "var dv = new DataView(new ArrayBuffer(4));
             dv.setUint32(0, 0x01020304, true);
             [dv.getUint32(0, true).toString(16),
              dv.getUint8(0).toString(16),
              dv.getUint8(3).toString(16)].join(',')"
        ),
        "1020304,4,1"
    );
    // Big-endian is the default and must round-trip too.
    assert_eq!(
        eval_string(
            "var dv = new DataView(new ArrayBuffer(4));
             dv.setUint32(0, 0x01020304, false);
             [dv.getUint32(0, false).toString(16),
              dv.getUint8(0).toString(16),
              dv.getUint8(3).toString(16)].join(',')"
        ),
        "1020304,1,4"
    );
}

#[test]
fn signed_and_float_widths() {
    // Negative values survive a round trip in both widths that have a sign.
    assert_eq!(
        eval_number("var dv = new DataView(new ArrayBuffer(2)); dv.setInt16(0, -1, true); dv.getInt16(0, true)"),
        -1.0
    );
    // float32 loses precision past 24 bits, but a small literal round-trips.
    assert_eq!(
        eval_number("var dv = new DataView(new ArrayBuffer(8)); dv.setFloat64(0, 1.5, true); dv.getFloat64(0, true)"),
        1.5
    );
    assert_eq!(
        eval_number("var dv = new DataView(new ArrayBuffer(4)); dv.setFloat32(0, 3.25, false); dv.getFloat32(0, false)"),
        3.25
    );
}

#[test]
fn getters_and_isview() {
    assert_eq!(
        eval_string(
            "var buf = new ArrayBuffer(8);
             var dv = new DataView(buf, 2, 4);
             [dv.byteOffset, dv.byteLength, typeof DataView, ArrayBuffer.isView(dv), dv.buffer === buf].join(',')"
        ),
        "2,4,function,true,true"
    );
}

#[test]
fn out_of_bounds_and_bad_args() {
    // Reads/writes must stay inside `[byteOffset, byteOffset + byteLength)`.
    assert_eq!(
        eval_string(
            "var r = [];
             var dv = new DataView(new ArrayBuffer(8));
             try { dv.getInt16(7); } catch (e) { r.push(e.name); }
             try { dv.setUint8(8, 0); } catch (e) { r.push(e.name); }
             r.join(',')"
        ),
        "RangeError,RangeError"
    );
    // The constructor validates its arguments.
    assert_eq!(
        eval_string(
            "var r = [];
             try { new DataView(5); } catch (e) { r.push(e.name); }
             try { new DataView(new ArrayBuffer(4), 5); } catch (e) { r.push(e.name); }
             try { new DataView(new ArrayBuffer(4), 2, 5); } catch (e) { r.push(e.name); }
             r.join(',')"
        ),
        "TypeError,RangeError,RangeError"
    );
}

#[test]
fn set_coerces_values_per_es_not_per_rust() {
    // `set*` 走的是 ES 的 `ToInt8` / `ToUint8` / …：先向零取整，再对 `2**bits` 取模
    // （有符号的把 ≥ 2**(bits-1) 的结果解释为负数），而 `NaN` / `±0` / `±∞` 都是 0。
    // Rust 的 `as` 是**饱和**转换，两者完全不同 —— `setInt8(0, 300)` 用 `as` 会写出
    // `127`，规范是 `300 mod 256 = 44`。
    assert_eq!(
        eval_string(
            "var dv = new DataView(new ArrayBuffer(8));
             var r = [];
             dv.setInt8(0, 300);    r.push(dv.getUint8(0));  // 44
             dv.setUint8(1, -1);    r.push(dv.getUint8(1));  // 255
             dv.setInt8(2, NaN);    r.push(dv.getUint8(2));  // 0
             dv.setInt16(4, 32768); r.push(dv.getInt16(4));  // -32768
             dv.setUint8(6, 255.9); r.push(dv.getUint8(6));  // 255（向零取整）
             r.join(',')"
        ),
        "44,255,0,-32768,255"
    );
}

#[test]
fn writes_land_in_the_underlying_buffer() {
    // A DataView is a window, not a copy: a byte written through it shows up in
    // the buffer (and in a sibling TypedArray view).
    assert_eq!(
        eval_string(
            "var buf = new ArrayBuffer(8);
             new DataView(buf).setUint8(3, 0xAA);
             new Uint8Array(buf)[3].toString(16)"
        ),
        "aa"
    );
}
