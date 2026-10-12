//! `ArrayBuffer` and the TypedArray views over it (ES 23.2 / 25.1).
//!
//! The bytes live in `ArrayBufferObject`; a view is `TypedArrayObject`, whose
//! `property_get("0")` decodes from the buffer. That aliasing is the whole point
//! of the feature (`new Uint8Array(buf)` and `buf` see each other's writes), and
//! it is why the element type lives in the VM rather than in a `Vec<Value>`.

use crate::RuntimeError;
use crate::vm::gc::{Gc, Heap, Object};
use crate::vm::object::{
    ArrayBufferObject, DataViewObject, JSObject, NativeFunctionObject, TypedArrayKind,
    TypedArrayObject,
};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// Getter names. A getter is reached as a *call* with the receiver as `this`, so
/// each one validates the receiver itself — the same shape `Map.prototype.size`
/// uses.
pub const TA_LENGTH_NATIVE: &str = "__ta_length__";
pub const TA_BYTE_LENGTH_NATIVE: &str = "__ta_byte_length__";
pub const TA_BYTE_OFFSET_NATIVE: &str = "__ta_byte_offset__";
pub const TA_BUFFER_NATIVE: &str = "__ta_buffer__";
pub const AB_BYTE_LENGTH_NATIVE: &str = "__ab_byte_length__";

/// `DataView` prototype getter names (reached as accessors, receiver arrives as
/// `this` — same shape as the TypedArray getters).
pub const DV_BUFFER_NATIVE: &str = "__dv_buffer__";
pub const DV_BYTE_LENGTH_NATIVE: &str = "__dv_byte_length__";
pub const DV_BYTE_OFFSET_NATIVE: &str = "__dv_byte_offset__";

/// Element type of a `DataView` access. Unlike `TypedArrayKind` these read *bytes*
/// at an absolute offset rather than elements, so the enum only needs the byte
/// width and the signedness of the integer forms.
#[derive(Clone, Copy)]
pub enum DataViewType {
    Int8,
    Uint8,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

impl DataViewType {
    fn bytes(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }

    /// Decode `data[..size]` (little- or big-endian) as this type.
    fn read(self, data: &[u8], little_endian: bool) -> f64 {
        let le = |b: [u8; 8], n: usize| -> [u8; 8] {
            if little_endian {
                b
            } else {
                let mut out = [0u8; 8];
                for i in 0..n {
                    out[i] = b[n - 1 - i];
                }
                out
            }
        };
        match self {
            Self::Int8 => data[0] as i8 as f64,
            Self::Uint8 => data[0] as f64,
            Self::Int16 => {
                i16::from_le_bytes(le([data[0], data[1], 0, 0, 0, 0, 0, 0], 2)[..2].try_into().unwrap())
                    as f64
            }
            Self::Uint16 => {
                u16::from_le_bytes(le([data[0], data[1], 0, 0, 0, 0, 0, 0], 2)[..2].try_into().unwrap())
                    as f64
            }
            Self::Int32 => {
                i32::from_le_bytes(le([data[0], data[1], data[2], data[3], 0, 0, 0, 0], 4)[..4].try_into().unwrap())
                    as f64
            }
            Self::Uint32 => {
                u32::from_le_bytes(le([data[0], data[1], data[2], data[3], 0, 0, 0, 0], 4)[..4].try_into().unwrap())
                    as f64
            }
            Self::Float32 => {
                f32::from_le_bytes(le([data[0], data[1], data[2], data[3], 0, 0, 0, 0], 4)[..4].try_into().unwrap())
                    as f64
            }
            Self::Float64 => f64::from_le_bytes(le(
                [
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ],
                8,
            )),
        }
    }

    /// Encode `value` (after ToNumber) into `data[..size]`, honoring endianness.
    fn write(self, data: &mut [u8], value: f64, little_endian: bool) {
        let bits: u64 = match self {
            Self::Int8 => to_int_n(value, 8, true),
            Self::Uint8 => to_int_n(value, 8, false),
            Self::Int16 => to_int_n(value, 16, true),
            Self::Uint16 => to_int_n(value, 16, false),
            Self::Int32 => to_int_n(value, 32, true),
            Self::Uint32 => to_int_n(value, 32, false),
            Self::Float32 => (value as f32).to_bits() as u64,
            Self::Float64 => value.to_bits(),
        };
        let bytes = bits.to_le_bytes();
        let n = self.bytes();
        for i in 0..n {
            data[i] = if little_endian { bytes[i] } else { bytes[n - 1 - i] };
        }
    }
}

/// `ToIndex` (ES 7.1.22) for a `DataView` offset / length argument: `undefined` /
/// `NaN` → 0, negatives and values above 2^53-1 are out of range.
/// ES 7.1.6–7.1.9 `ToInt8` / `ToUint8` / `ToInt16` / `ToUint16` / `ToInt32` / `ToUint32`.
///
/// All six share one shape: `NaN`, `±0` and `±∞` become `0`; otherwise the value is
/// truncated toward zero and taken modulo `2**bits`, and the signed variants
/// re-interpret results ≥ `2**(bits-1)` as negative. Rust's `as` cast does
/// *neither* — it saturates — which is why `setInt8(0, 300)` used to write `127`
/// instead of `44`, and `setUint8(0, -1)` wrote `0` instead of `255`.
fn to_int_n(value: f64, bits: u32, signed: bool) -> u64 {
    if value.is_nan() || value.is_infinite() || value == 0.0 {
        return 0;
    }
    let modulus = (1u64 << bits) as f64;
    let wrapped = value.trunc().rem_euclid(modulus);
    if signed && wrapped >= modulus / 2.0 {
        (wrapped - modulus) as i64 as u64
    } else {
        wrapped as u64
    }
}

fn to_index(value: Option<&Value>) -> Result<usize, RuntimeError> {
    let n = match value {
        None | Some(Value::Undefined) | Some(Value::Null) => 0.0,
        Some(v) => v.to_number(),
    };
    if n.is_nan() {
        return Ok(0);
    }
    if n < 0.0 {
        return Err(RuntimeError::RangeError(
            "DataView offset must not be negative".to_string(),
        ));
    }
    if n > 9_007_199_254_740_991.0 {
        return Err(RuntimeError::RangeError(
            "DataView offset is too large".to_string(),
        ));
    }
    Ok(n as usize)
}

/// Every TypedArray constructor the engine provides. `%TypedArray%` itself is
/// not exposed (`BigInt64Array` / `BigUint64Array` are: BigInt is still absent).
pub const TYPED_ARRAY_KINDS: [TypedArrayKind; 9] = [
    TypedArrayKind::Int8,
    TypedArrayKind::Uint8,
    TypedArrayKind::Uint8Clamped,
    TypedArrayKind::Int16,
    TypedArrayKind::Uint16,
    TypedArrayKind::Int32,
    TypedArrayKind::Uint32,
    TypedArrayKind::Float32,
    TypedArrayKind::Float64,
];

/// Native name of the `get Uint8Array[Symbol.species]` accessor (ES 23.2.2.2):
/// 同 `Map` / `Set` / `Array` 的 species，取值器返回它的接收者。
pub const TA_SPECIES_NATIVE: &str = "__typedarray_species__";

/// The kind whose constructor is `name`, so the VM can route `new Uint8Array(…)`
/// without a match of its own.
pub fn kind_from_name(name: &str) -> Option<TypedArrayKind> {
    TYPED_ARRAY_KINDS.iter().copied().find(|k| k.name() == name)
}

pub fn is_typedarray_receiver(value: &Value) -> bool {
    matches!(value, Value::Object(obj_ref)
        if obj_ref.borrow().is_typed_array())
}

pub fn is_arraybuffer_receiver(value: &Value) -> bool {
    matches!(value, Value::Object(obj_ref)
        if obj_ref.borrow().is_array_buffer())
}

pub fn is_dataview_receiver(value: &Value) -> bool {
    matches!(value, Value::Object(obj_ref)
        if obj_ref.borrow().is_data_view())
}

fn as_dataview<'a>(
    this: &'a Value,
    method: &str,
) -> Result<std::cell::RefMut<'a, DataViewObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "{method} called on a non-object"
        )));
    };
    if !obj_ref.borrow().is_data_view() {
        return Err(RuntimeError::TypeError(format!(
            "{method} called on an object that is not a DataView"
        )));
    }
    Ok(std::cell::RefMut::map(obj_ref.borrow_mut(), |obj| {
        obj.as_data_view_mut()
            .expect("checked above")
    }))
}

fn as_typedarray<'a>(this: &'a Value, method: &str) -> Result<std::cell::RefMut<'a, TypedArrayObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "{method} called on a non-object"
        )));
    };
    let is_ta = obj_ref.borrow().is_typed_array();
    if !is_ta {
        return Err(RuntimeError::TypeError(format!(
            "{method} called on an object that is not a TypedArray"
        )));
    }
    let borrowed = obj_ref.borrow_mut();
    Ok(std::cell::RefMut::map(borrowed, |obj| {
        obj.as_typed_array_mut()
            .expect("checked above")
    }))
}

/// 一块 Data Block 允许的最大字节数。**超过它不是错误，而是 range error** ——
/// ES 25.1.1.1 `AllocateArrayBuffer` 明确允许实现在创建不出来的时候抛 `RangeError`；
/// 真实引擎也是这么做的（`built-ins/ArrayBuffer/allocation-limit.js` 连
/// "7 PiB 应当 RangeError" 都在断言里写明了）。
///
/// 这不是洁癖：实测 `new ArrayBuffer(7 * 1024**5)` 与 `new Uint8Array(2**32)`
/// 之前会**真的去要 4GB / 7.8PB**，结果是进程 SIGABRT —— 而"崩溃优先于失败"
/// 是本项目的硬纪律（handover §6 第 3 条），一条用例就把整个分片的结果带走。
const MAX_BUFFER_BYTES: usize = 1 << 30;

/// 一次尺寸请求是否合法 + 分配得出来。两件事分开是为了让错误信息能指出区别。
fn checked_byte_length(requested: f64, bytes_per_element: usize) -> Result<usize, RuntimeError> {
    // ES 7.1.22 `ToIndex`：NaN → 0；负数 / +∞ / 超过 2^53-1 → RangeError。
    if requested.is_nan() {
        return Ok(0);
    }
    if requested < 0.0 {
        return Err(RuntimeError::RangeError(
            "Invalid array buffer length".to_string(),
        ));
    }
    let count = requested.trunc();
    if count > 9_007_199_254_740_991.0 {
        return Err(RuntimeError::RangeError(
            "Array buffer length exceeds the safe integer range".to_string(),
        ));
    }
    let count = count as usize;
    let total = count
        .checked_mul(bytes_per_element)
        .unwrap_or(usize::MAX);
    if total > MAX_BUFFER_BYTES {
        return Err(RuntimeError::RangeError(
            "Array buffer allocation failed".to_string(),
        ));
    }
    Ok(total)
}

/// `new ArrayBuffer(byteLength)`.
pub fn arraybuffer_construct(
    heap: &mut Heap,
    args: &[Value],
    proto: Option<Value>,
) -> Result<Value, RuntimeError> {
    let requested = args.first().map(|v| v.to_number()).unwrap_or(0.0);
    let byte_length = checked_byte_length(requested, 1)?;
    Ok(make_buffer(heap, byte_length, proto))
}

fn make_buffer(heap: &mut Heap, byte_length: usize, proto: Option<Value>) -> Value {
    let mut buffer = ArrayBufferObject::new(byte_length, None);
    if let Some(Value::Object(p)) = proto {
        buffer.set_prototype(Some(p));
    }
    Value::Object(heap.alloc(Object::ArrayBuffer(buffer)))
}

/// `new DataView(buffer [, byteOffset [, byteLength]])` (ES 25.3.2.1).
pub fn dataview_construct(
    heap: &mut Heap,
    args: &[Value],
    proto: Option<Value>,
) -> Result<Value, RuntimeError> {
    let first = args.first().cloned().unwrap_or(Value::Undefined);
    if !is_arraybuffer_receiver(&first) {
        return Err(RuntimeError::TypeError(
            "DataView requires an ArrayBuffer argument".to_string(),
        ));
    }
    let (buffer_len, buffer_obj) = {
        let Value::Object(obj_ref) = &first else {
            unreachable!()
        };
        let borrowed = obj_ref.borrow();
        let Some(buffer) = borrowed.as_array_buffer() else {
            return Err(RuntimeError::TypeError(
                "DataView requires an ArrayBuffer argument".to_string(),
            ));
        };
        if buffer.is_detached() {
            return Err(RuntimeError::TypeError(
                "DataView cannot view a detached ArrayBuffer".to_string(),
            ));
        }
        (buffer.byte_length(), *obj_ref)
    };
    let offset = to_index(args.get(1))?;
    if offset > buffer_len {
        return Err(RuntimeError::RangeError(
            "DataView byteOffset is outside the bounds of the buffer".to_string(),
        ));
    }
    let length = match args.get(2) {
        None | Some(Value::Undefined) => buffer_len - offset,
        Some(v) => {
            let len = to_index(Some(v))?;
            if offset + len > buffer_len {
                return Err(RuntimeError::RangeError(
                    "DataView byteLength is outside the bounds of the buffer".to_string(),
                ));
            }
            len
        }
    };
    let mut dv = DataViewObject::new(buffer_obj, offset, length, None);
    if let Some(Value::Object(p)) = proto {
        dv.set_prototype(Some(p));
    }
    Ok(Value::Object(heap.alloc(Object::DataView(dv))))
}

// ── getters ──────────────────────────────────────────────────────────────────

/// Validate `this` is a live (non-detached) DataView, returning the borrowed view.
/// The getters all throw on a detached buffer (ES 25.3.1.3-5).
fn as_live_dataview<'a>(
    this: &'a Value,
) -> Result<std::cell::RefMut<'a, DataViewObject>, RuntimeError> {
    let dv = as_dataview(this, "DataView getter")?;
    if dv.is_detached() {
        return Err(RuntimeError::TypeError(
            "DataView access after detachment".to_string(),
        ));
    }
    Ok(dv)
}

pub fn dv_buffer(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Object(as_live_dataview(this)?.buffer))
}

pub fn dv_byte_length(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(as_live_dataview(this)?.byte_length as f64))
}

pub fn dv_byte_offset(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(as_live_dataview(this)?.byte_offset as f64))
}

/// `getInt8(byteOffset, littleEndian?)` … `getFloat64(byteOffset, littleEndian?)`:
/// decode `size` bytes from `buffer[byteOffset + byteOffset ..]` honoring endianness.
pub fn dv_get(this: &Value, args: &[Value], ty: DataViewType) -> Result<Value, RuntimeError> {
    let dv = as_live_dataview(this)?;
    let offset = to_index(args.first())?;
    let size = ty.bytes();
    if offset + size > dv.byte_length {
        return Err(RuntimeError::RangeError(
            "DataView read is outside the bounds of the buffer".to_string(),
        ));
    }
    let little = args.get(1).map(|v| v.to_boolean()).unwrap_or(false);
    let buffer = dv.buffer.borrow();
    let Some(abuf) = buffer.as_array_buffer() else {
        return Err(RuntimeError::TypeError(
            "DataView buffer is not an ArrayBuffer".to_string(),
        ));
    };
    let at = dv.byte_offset + offset;
    let data = abuf.data();
    Ok(Value::Number(ty.read(&data[at..at + size], little)))
}

/// `setInt8(byteOffset, value, littleEndian?)` … `setFloat64(…)`: the inverse of
/// `dv_get`.
pub fn dv_set(this: &Value, args: &[Value], ty: DataViewType) -> Result<Value, RuntimeError> {
    let dv = as_live_dataview(this)?;
    let offset = to_index(args.first())?;
    let size = ty.bytes();
    if offset + size > dv.byte_length {
        return Err(RuntimeError::RangeError(
            "DataView write is outside the bounds of the buffer".to_string(),
        ));
    }
    let value = args.get(1).cloned().unwrap_or(Value::Undefined).to_number();
    let little = args.get(2).map(|v| v.to_boolean()).unwrap_or(false);
    let mut buffer = dv.buffer.borrow_mut();
    let Some(abuf) = buffer.as_array_buffer_mut() else {
        return Err(RuntimeError::TypeError(
            "DataView buffer is not an ArrayBuffer".to_string(),
        ));
    };
    let at = dv.byte_offset + offset;
    let data = abuf.data_mut();
    ty.write(&mut data[at..at + size], value, little);
    Ok(Value::Undefined)
}

/// The numbers a TypedArray constructor should copy: the elements of another
/// TypedArray (read as elements, not as properties — `length` is a prototype
/// getter and would read as 0) or of an array-like.
///
/// `bytes_per_element` 是进来就先的那道闸：类数组的 `length` 也是**请求尺寸**
/// 的一种写法（`new Uint8Array({ length: 2 ** 31 })`），所以它也必须在
/// `MAX_BUFFER_BYTES` 之前被拦住 —— 实测 `length-excessive-throws.js` 就是靠这个
/// 形状把整个分片干掉的（2GiB）。
fn source_numbers(value: &Value, bytes_per_element: usize) -> Result<Vec<f64>, RuntimeError> {
    let mut out = Vec::new();
    let Value::Object(obj_ref) = value else {
        return Ok(out);
    };
    // A TypedArray source: read through the view.
    if obj_ref.borrow().is_typed_array() {
        let ta = obj_ref.borrow();
        let ta = ta.as_typed_array()
            .expect("checked above");
        for i in 0..ta.length {
            out.push(match ta.get_element(i) {
                Value::Number(n) => n,
                other => other.to_number(),
            });
        }
        return Ok(out);
    }
    let borrowed = obj_ref.borrow();
    let length = borrowed
        .property_get(&PropertyKey::from_str("length"))
        .map(|d| d.value.to_number())
        .unwrap_or(0.0);
    if length.is_nan() || length <= 0.0 {
        return Ok(out);
    }
    let length = checked_byte_length(length, bytes_per_element)? / bytes_per_element;
    for i in 0..length {
        let element = borrowed
            .property_get(&PropertyKey::from_str(&i.to_string()))
            .map(|d| d.value.to_number())
            .unwrap_or(f64::NAN);
        out.push(element);
    }
    Ok(out)
}

/// `new Uint8Array(…)` and friends (ES 23.2.5.1): a length, another TypedArray,
/// an array-like / iterable, or a buffer with an optional byte offset and length.
pub fn typedarray_construct(
    heap: &mut Heap,
    kind: TypedArrayKind,
    args: &[Value],
    proto: Option<Value>,
) -> Result<Value, RuntimeError> {
    let size = kind.bytes_per_element();
    let first = args.first().cloned().unwrap_or(Value::Undefined);

    // `new TA(buffer, byteOffset, length)` — a *view*, not a copy.
    if is_arraybuffer_receiver(&first) {
        let Value::Object(buffer) = first else {
            unreachable!()
        };
        let buffer_len = {
            let borrowed = buffer.borrow();
            borrowed.as_array_buffer()
                .map(|b| b.byte_length())
                .unwrap_or(0)
        };
        let offset = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
        let offset = if offset.is_nan() { 0.0 } else { offset };
        if offset < 0.0 || offset as usize > buffer_len {
            return Err(RuntimeError::RangeError(
                "Invalid typed array byte offset".to_string(),
            ));
        }
        let mut byte_offset = offset as usize;
        // A byte offset that is not a multiple of the element size is a RangeError
        // (ES 23.2.5.1 step 12).
        if byte_offset % size != 0 {
            return Err(RuntimeError::RangeError(
                "Start offset must be a multiple of the element size".to_string(),
            ));
        }
        let available = buffer_len.saturating_sub(byte_offset) / size;
        let length = match args.get(2) {
            None => available,
            Some(v) => {
                let requested = v.to_number();
                if requested.is_nan() {
                    0
                } else if requested < 0.0 || requested as usize > available {
                    return Err(RuntimeError::RangeError(
                        "Invalid typed array length".to_string(),
                    ));
                } else {
                    requested as usize
                }
            }
        };
        let _ = byte_offset;
        byte_offset = byte_offset.min(buffer_len);
        return Ok(make_view(heap, kind, buffer, byte_offset, length, proto));
    }

    // Anything else is a length or something to copy — both get their own buffer.
    let numbers = match &first {
        Value::Undefined => Vec::new(),
        // A number (or a numeric string) is a *length*; an object is a source to
        // copy, which is why the two cases cannot share a branch.
        other if !matches!(other, Value::Object(_)) => {
            let requested = other.to_number();
            if requested.is_nan() {
                Vec::new()
            } else if requested < 0.0 || requested.fract() != 0.0 {
                return Err(RuntimeError::RangeError(
                    "Invalid typed array length".to_string(),
                ));
            } else {
                // 同上：`new Uint8Array(2**32)` 以前会真的去要 4GB。
                // `checked_byte_length` 同时负责 ToIndex 的上界与"分配不出来"。
                let length = checked_byte_length(requested, size)? / size;
                vec![0.0; length]
            }
        }
        other => source_numbers(other, size)?,
    };

    let buffer = make_buffer(heap, numbers.len() * size, None);
    let Value::Object(buffer_obj) = buffer else {
        unreachable!()
    };
    let view = make_view(heap, kind, buffer_obj, 0, numbers.len(), proto);
    if let Value::Object(view_ref) = &view {
        let mut view_mut = view_ref.borrow_mut();
        if let Some(ta) = view_mut.as_typed_array_mut() {
            for (i, value) in numbers.iter().enumerate() {
                // The buffer is borrowed by the view only through `Rc`, so this
                // borrow is safe.
                let mut buffer_mut = buffer_obj.borrow_mut();
                if let Some(buf) = buffer_mut.as_array_buffer_mut() {
                    ta.kind.write(buf.data_mut(), i, *value);
                }
            }
        }
    }
    Ok(view)
}

fn make_view(
    heap: &mut Heap,
    kind: TypedArrayKind,
    buffer: Gc<Object>,
    byte_offset: usize,
    length: usize,
    proto: Option<Value>,
) -> Value {
    let mut view = TypedArrayObject::new(kind, buffer, byte_offset, length, None);
    if let Some(Value::Object(p)) = proto {
        view.set_prototype(Some(p));
    }
    Value::Object(heap.alloc(Object::TypedArray(view)))
}

// ── getters ────────────────────────────────────────────────────────────────

pub fn ta_length(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(as_typedarray(this, "length")?.length as f64))
}

pub fn ta_byte_length(this: &Value) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "byteLength")?;
    Ok(Value::Number((ta.length * ta.kind.bytes_per_element()) as f64))
}

pub fn ta_byte_offset(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(
        as_typedarray(this, "byteOffset")?.byte_offset as f64,
    ))
}

pub fn ta_buffer(this: &Value) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "buffer")?;
    Ok(Value::Object(ta.buffer))
}

pub fn ab_byte_length(this: &Value) -> Result<Value, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(
            "byteLength called on a non-object".to_string(),
        ));
    };
    let borrowed = obj_ref.borrow();
    let Some(buffer) = borrowed.as_array_buffer() else {
        return Err(RuntimeError::TypeError(
            "byteLength called on an object that is not an ArrayBuffer".to_string(),
        ));
    };
    Ok(Value::Number(buffer.byte_length() as f64))
}

// ── methods ────────────────────────────────────────────────────────────────

/// The elements of a TypedArray, as `Array.from` needs them.
///
/// A view is array-like, but its `length` is a *prototype getter*: reading it as
/// an own property (which is all the builtin layer can do) answers `undefined`,
/// and `Array.from` then produced `[]`. `Some` for a TypedArray, `None` for
/// anything else.
pub fn typedarray_elements(value: &Value) -> Option<Vec<Value>> {
    let Value::Object(obj_ref) = value else {
        return None;
    };
    let borrowed = obj_ref.borrow();
    let ta = borrowed.as_typed_array()?;
    Some((0..ta.length).map(|i| ta.get_element(i)).collect())
}

/// `set(source, offset)` — copies into this view.
pub fn ta_set(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let source = args.first().cloned().unwrap_or(Value::Undefined);
    let numbers = match source {
        // A TypedArray source copies *elements*, even of a different kind.
        other if is_typedarray_receiver(&other) => source_numbers(&other, 1)?,
        Value::Object(_) | Value::String(_) => source_numbers(&source, 1)?,
        other => return Err(RuntimeError::TypeError(format!(
            "TypedArray.prototype.set: {} is not an array-like",
            other.type_of()
        ))),
    };
    let offset = args.get(1).map(|v| v.to_number()).unwrap_or(0.0);
    let offset = if offset.is_nan() { 0.0 } else { offset };
    if offset < 0.0 || offset.fract() != 0.0 {
        return Err(RuntimeError::RangeError(
            "Invalid offset".to_string(),
        ));
    }
    let mut ta = as_typedarray(this, "set")?;
    let start = offset as usize;
    if start + numbers.len() > ta.length {
        return Err(RuntimeError::RangeError(
            "Source is too large".to_string(),
        ));
    }
    let kind = ta.kind;
    let byte_offset = ta.byte_offset;
    let buffer = ta.buffer;
    drop(ta);
    let mut buffer_mut = buffer.borrow_mut();
    let Some(buf) = buffer_mut.as_array_buffer_mut() else {
        return Ok(Value::Undefined);
    };
    // Write at the view's own offset within the shared byte block.
    let data = buf.data_mut();
    for (i, value) in numbers.iter().enumerate() {
        let at = byte_offset + (start + i) * kind.bytes_per_element();
        if at + kind.bytes_per_element() > data.len() {
            break;
        }
        kind.write(&mut data[at..], 0, *value);
    }
    Ok(Value::Undefined)
}

/// `subarray(begin, end)` — a new view over the *same* buffer.
pub fn ta_subarray(heap: &mut Heap, this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "subarray")?;
    let length = ta.length;
    let begin = relative_index(args.first(), 0, length);
    let end = relative_index(args.get(1), length, length);
    let new_length = end.saturating_sub(begin);
    let kind = ta.kind;
    let byte_offset = ta.byte_offset + begin * kind.bytes_per_element();
    let buffer = ta.buffer;
    let proto = ta.get_prototype().map(Value::Object);
    drop(ta);
    Ok(make_view(heap, kind, buffer, byte_offset, new_length, proto))
}

/// `slice(begin, end)` — a *copy* into a fresh buffer.
pub fn ta_slice(heap: &mut Heap, this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "slice")?;
    let length = ta.length;
    let begin = relative_index(args.first(), 0, length);
    let end = relative_index(args.get(1), length, length);
    let new_length = end.saturating_sub(begin);
    let numbers: Vec<f64> = (0..new_length)
        .map(|i| match ta.get_element(begin + i) {
            Value::Number(n) => n,
            other => other.to_number(),
        })
        .collect();
    let kind = ta.kind;
    let proto = ta.get_prototype().map(Value::Object);
    drop(ta);
    let buffer = make_buffer(heap, new_length * kind.bytes_per_element(), None);
    let Value::Object(buffer_obj) = buffer else {
        unreachable!()
    };
    let view = make_view(heap, kind, buffer_obj, 0, new_length, proto);
    if let Value::Object(view_ref) = &view {
        let mut view_mut = view_ref.borrow_mut();
        let ta_mut = view_mut.as_typed_array_mut();
        let mut buffer_mut = buffer_obj.borrow_mut();
        let buf = buffer_mut.as_array_buffer_mut();
        if let (Some(ta_mut), Some(buf)) = (ta_mut, buf) {
            for (i, value) in numbers.iter().enumerate() {
                ta_mut.kind.write(buf.data_mut(), i, *value);
            }
        }
    }
    Ok(view)
}

/// `fill(value, start, end)`.
pub fn ta_fill(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
    let ta = as_typedarray(this, "fill")?;
    let length = ta.length;
    let start = relative_index(args.get(1), 0, length);
    let end = relative_index(args.get(2), length, length);
    let kind = ta.kind;
    let byte_offset = ta.byte_offset;
    let buffer = ta.buffer;
    drop(ta);
    let mut buffer_mut = buffer.borrow_mut();
    if let Some(buf) = buffer_mut.as_array_buffer_mut() {
        for i in start..end {
            let at = byte_offset + i * kind.bytes_per_element();
            if at + kind.bytes_per_element() > buf.data().len() {
                break;
            }
            kind.write(&mut buf.data_mut()[at..], 0, value);
        }
    }
    Ok(this.clone())
}

// ── `%TypedArray%.prototype` 的只读 / 原地方法 ──
//
// 这几个都不需要调用户代码（所以不需要 VM，只要 `heap`/`this` 就够），可以像
// `ta_fill` 那样在 `call_prototype_method` 里直接分派。需要**回调**的那一族
// （`map` / `filter` / `reduce` …）必须走 VM，另做。

/// 严格相等（ES `IsStrictlyEqual`）在 TypedArray 上的形态：元素恒为 Number，
/// 所以只要 needle 不是 Number 就永假；`NaN !== NaN` 由 f64 的 `==` 天然给出。
fn number_needle(args: &[Value]) -> Option<f64> {
    match args.first() {
        Some(Value::Number(n)) => Some(*n),
        _ => None,
    }
}

/// `indexOf(searchElement, fromIndex)` —— 严格相等，找不到返回 `-1`。
pub fn ta_index_of(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(needle) = number_needle(args) else {
        return Ok(Value::Number(-1.0));
    };
    let ta = as_typedarray(this, "indexOf")?;
    let len = ta.length;
    let from = relative_index(args.get(1), 0, len);
    for i in from..len {
        if let Value::Number(n) = ta.get_element(i) {
            if n == needle {
                return Ok(Value::Number(i as f64));
            }
        }
    }
    Ok(Value::Number(-1.0))
}

/// `lastIndexOf(searchElement, fromIndex)` —— 从 `fromIndex` 往左找。
///
/// `fromIndex` 缺省是 `length - 1`；为负是 `length + fromIndex`；大于 `length - 1`
/// 夹到 `length - 1`（ES 23.2.3.19）。
pub fn ta_last_index_of(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(needle) = number_needle(args) else {
        return Ok(Value::Number(-1.0));
    };
    let ta = as_typedarray(this, "lastIndexOf")?;
    let len = ta.length;
    let from = match args.get(1) {
        None => len as isize - 1,
        Some(v) => {
            let raw = v.to_number();
            if raw.is_nan() {
                len as isize - 1
            } else {
                let n = raw.trunc();
                if n < 0.0 {
                    len as isize + n as isize
                } else {
                    (n as isize).min(len as isize - 1)
                }
            }
        }
    };
    let mut i = from;
    while i >= 0 {
        if let Value::Number(n) = ta.get_element(i as usize) {
            if n == needle {
                return Ok(Value::Number(i as f64));
            }
        }
        i -= 1;
    }
    Ok(Value::Number(-1.0))
}

/// `includes(searchElement, fromIndex)` —— **SameValueZero**，`NaN` 能找到自己
/// （与 `indexOf` 的严格相等不同，这是两者唯一的分歧点）。
pub fn ta_includes(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let Some(needle) = number_needle(args) else {
        return Ok(Value::Bool(false));
    };
    let ta = as_typedarray(this, "includes")?;
    let len = ta.length;
    let from = relative_index(args.get(1), 0, len);
    for i in from..len {
        if let Value::Number(n) = ta.get_element(i) {
            if n == needle || (n.is_nan() && needle.is_nan()) {
                return Ok(Value::Bool(true));
            }
        }
    }
    Ok(Value::Bool(false))
}

/// `join(separator)` —— 分隔符缺省 `","`；`undefined` / `null` 元素当空串。
pub fn ta_join(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "join")?;
    let len = ta.length;
    let sep = match args.first() {
        None | Some(Value::Undefined) => ",".to_string(),
        Some(v) => v.to_js_string(),
    };
    let mut out = String::new();
    for i in 0..len {
        if i > 0 {
            out.push_str(&sep);
        }
        match ta.get_element(i) {
            Value::Undefined | Value::Null => {}
            v => out.push_str(&v.to_js_string()),
        }
    }
    Ok(Value::string(&out))
}

/// `reverse()` —— **原地**反转，返回 `this`（不是副本）。
pub fn ta_reverse(this: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    let mut ta = as_typedarray(this, "reverse")?;
    let len = ta.length;
    let mut vals: Vec<Value> = (0..len).map(|i| ta.get_element(i)).collect();
    vals.reverse();
    for (i, v) in vals.into_iter().enumerate() {
        ta.set_element(i, v);
    }
    drop(ta);
    Ok(this.clone())
}

/// `at(index)` —— 负索引从末尾算；越界是 `undefined`。
pub fn ta_at(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let ta = as_typedarray(this, "at")?;
    let len = ta.length;
    let raw = args.first().map(|v| v.to_number()).unwrap_or(0.0);
    let idx = if raw.is_nan() { 0.0 } else { raw.trunc() };
    let i = if idx < 0.0 { len as f64 + idx } else { idx };
    if i < 0.0 || i >= len as f64 {
        return Ok(Value::Undefined);
    }
    Ok(ta.get_element(i as usize))
}

/// `copyWithin(target, start, end)` —— **原地**搬运，返回 `this`。
///
/// 源区间与目标区间可能重叠，所以先整段读出来再写回。
pub fn ta_copy_within(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut ta = as_typedarray(this, "copyWithin")?;
    let len = ta.length;
    let target = relative_index(args.first(), 0, len);
    let start = relative_index(args.get(1), 0, len);
    let end = relative_index(args.get(2), len, len);
    let count = end.saturating_sub(start).min(len.saturating_sub(target));
    let src: Vec<Value> = (start..start + count).map(|i| ta.get_element(i)).collect();
    for (k, v) in src.into_iter().enumerate() {
        ta.set_element(target + k, v);
    }
    drop(ta);
    Ok(this.clone())
}

/// `ArrayBuffer.prototype.slice(start, end)` — a copy into a new buffer.
pub fn ab_slice(heap: &mut Heap, this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(
            "slice called on a non-object".to_string(),
        ));
    };
    let (data, proto) = {
        let borrowed = obj_ref.borrow();
        let Some(buffer) = borrowed.as_array_buffer() else {
            return Err(RuntimeError::TypeError(
                "slice called on an object that is not an ArrayBuffer".to_string(),
            ));
        };
        (buffer.data().to_vec(), borrowed.get_prototype().map(Value::Object))
    };
    let length = data.len();
    let start = relative_index(args.first(), 0, length);
    let end = relative_index(args.get(1), length, length);
    let copy = data[start..end].to_vec();
    let new_buffer = make_buffer(heap, copy.len(), proto);
    if let (Value::Object(new_ref), Value::Object(_)) = (&new_buffer, this) {
        let mut borrowed = new_ref.borrow_mut();
        if let Some(buffer) = borrowed.as_array_buffer_mut() {
            buffer.data_mut().copy_from_slice(&copy);
        }
    }
    Ok(new_buffer)
}

/// `ArrayBuffer.isView(value)` — TypedArray and DataView both count.
pub fn arraybuffer_is_view(args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Bool(args.first().is_some_and(|v| {
        is_typedarray_receiver(v) || is_dataview_receiver(v)
    })))
}

/// `start` as an index into `[0, length]`, clamping the way `Array.prototype`
/// does and turning negative values into "from the end".
fn relative_index(argument: Option<&Value>, default: usize, length: usize) -> usize {
    let value = argument.map(|v| v.to_number()).unwrap_or(default as f64);
    let index = if value.is_nan() { default as f64 } else { value };
    let index = index.trunc();
    let index = if index < 0.0 {
        (length as f64 + index).max(0.0)
    } else {
        index.min(length as f64)
    };
    index as usize
}

// ── prototypes ─────────────────────────────────────────────────────────────

pub fn register_arraybuffer_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_prototype_method;
    set_prototype_method(proto, heap, "slice", |_this, _args| Ok(Value::Undefined));
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("byteLength"),
        accessor(heap, AB_BYTE_LENGTH_NATIVE),
    );
}

/// `DataView.prototype`: the 8 `get*` / 8 `set*` methods and the three accessors
/// (`buffer` / `byteLength` / `byteOffset`).
pub fn register_dataview_prototype(proto: &Gc<Object>, heap: &mut Heap) {
    use super::set_prototype_method;
    let get_types: [(DataViewType, &str); 8] = [
        (DataViewType::Int8, "getInt8"),
        (DataViewType::Uint8, "getUint8"),
        (DataViewType::Int16, "getInt16"),
        (DataViewType::Uint16, "getUint16"),
        (DataViewType::Int32, "getInt32"),
        (DataViewType::Uint32, "getUint32"),
        (DataViewType::Float32, "getFloat32"),
        (DataViewType::Float64, "getFloat64"),
    ];
    for (ty, name) in get_types {
        let ty = ty;
        set_prototype_method(proto, heap, name, move |this, args| dv_get(this, args, ty));
    }
    let set_types: [(DataViewType, &str); 8] = [
        (DataViewType::Int8, "setInt8"),
        (DataViewType::Uint8, "setUint8"),
        (DataViewType::Int16, "setInt16"),
        (DataViewType::Uint16, "setUint16"),
        (DataViewType::Int32, "setInt32"),
        (DataViewType::Uint32, "setUint32"),
        (DataViewType::Float32, "setFloat32"),
        (DataViewType::Float64, "setFloat64"),
    ];
    for (ty, name) in set_types {
        let ty = ty;
        set_prototype_method(proto, heap, name, move |this, args| dv_set(this, args, ty));
    }
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("buffer"),
        accessor(heap, DV_BUFFER_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("byteLength"),
        accessor(heap, DV_BYTE_LENGTH_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("byteOffset"),
        accessor(heap, DV_BYTE_OFFSET_NATIVE),
    );
}

pub fn register_arraybuffer_statics(ctor: &Value) {
    let _ = ctor;
}

/// One TypedArray prototype: the shared methods, the four getters,
/// `BYTES_PER_ELEMENT`, and `Symbol.iterator` (the generic factory: a view is
/// array-like, so iterating it reads `length` and the indices).
pub fn register_typedarray_prototype(
    kind: TypedArrayKind,
    proto: &Gc<Object>,
    heap: &mut Heap,
) {
    use super::set_prototype_method;
    set_prototype_method(proto, heap, "set", |this, args| ta_set(this, args));
    set_prototype_method(proto, heap, "subarray", |_this, _args| Ok(Value::Undefined));
    set_prototype_method(proto, heap, "slice", |_this, _args| Ok(Value::Undefined));
    set_prototype_method(proto, heap, "fill", |this, args| ta_fill(this, args));
    // 只读 / 原地的一族（不需要回调 ⇒ 不需要 VM）：
    set_prototype_method(proto, heap, "indexOf", |this, args| ta_index_of(this, args));
    set_prototype_method(proto, heap, "lastIndexOf", |this, args| {
        ta_last_index_of(this, args)
    });
    set_prototype_method(proto, heap, "includes", |this, args| ta_includes(this, args));
    set_prototype_method(proto, heap, "join", |this, args| ta_join(this, args));
    set_prototype_method(proto, heap, "reverse", |this, args| ta_reverse(this, args));
    set_prototype_method(proto, heap, "at", |this, args| ta_at(this, args));
    set_prototype_method(proto, heap, "copyWithin", |this, args| {
        ta_copy_within(this, args)
    });

    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("length"),
        accessor(heap, TA_LENGTH_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("byteLength"),
        accessor(heap, TA_BYTE_LENGTH_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("byteOffset"),
        accessor(heap, TA_BYTE_OFFSET_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("buffer"),
        accessor(heap, TA_BUFFER_NATIVE),
    );
    let _ = proto.borrow_mut().define_property(
        PropertyKey::from_str("BYTES_PER_ELEMENT"),
        data(Value::Number(kind.bytes_per_element() as f64)),
    );
    // Iterating a view yields its elements: the factory reads `length` (a getter
    // here, which the VM dispatches) and then the indices.
    let _ = proto.borrow_mut().define_property(
        crate::vm::iterator::iterator_symbol_key(),
        super::method_descriptor(Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(crate::vm::iterator::ITERATOR_NATIVE_NAME),
        )))),
    );
}

/// A non-enumerable, non-configurable data property — the attributes
/// `BYTES_PER_ELEMENT` carries (ES 23.2.5.3).
fn data(value: Value) -> PropertyDescriptor {
    PropertyDescriptor {
        value,
        writable: false,
        enumerable: false,
        configurable: false,
        getter: None,
        setter: None,
    }
}

/// An accessor with only a getter, as every `length`-style property is.
fn accessor(heap: &mut Heap, getter_name: &str) -> PropertyDescriptor {
    PropertyDescriptor {
        value: Value::Undefined,
        writable: true,
        enumerable: false,
        configurable: true,
        getter: Some(Value::Object(heap.alloc(Object::NativeFunction(
            NativeFunctionObject::new(getter_name),
        )))),
        setter: None,
    }
}
