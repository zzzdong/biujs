//! `Date` (ES 21.4).
//!
//! The everyday surface: the constructor in all its forms, `Date.now/parse/UTC`,
//! the local and UTC component getters, a few setters, and the string forms.
//! Anything locale-dependent (`toLocaleDateString` with a real locale, the
//! timezone *name* in `toString`) is out of reach without a locale database, so
//! those degrade to their spec-shaped defaults.
//!
//! The `[[DateValue]]` slot lives on `vm::object::DateObject`; local time comes
//! from `chrono`, which is the only place the process's timezone is available.

use std::cell::{RefCell, RefMut};
use std::rc::Rc;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike, Utc};

use crate::RuntimeError;
use crate::vm::object::{DateObject, JSObject, NativeFunctionObject};
use crate::vm::property::{PropertyDescriptor, PropertyKey};
use crate::vm::value::Value;

/// True when `this` is a `Date` instance.
///
/// Needed as a dispatch guard: `valueOf` / `toString` exist on every object, so
/// the shared prototype-method dispatch may only claim them for a real Date.
pub fn is_date_receiver(this: &Value) -> bool {
    matches!(this, Value::Object(obj)
        if obj.borrow().kind() == crate::vm::property::ObjectKind::Date)
}

/// Downcast a receiver to `DateObject`, raising the TypeError the spec asks for
/// when a Date method is applied to something else.
fn as_date<'a>(this: &'a Value, method: &str) -> Result<RefMut<'a, DateObject>, RuntimeError> {
    let Value::Object(obj_ref) = this else {
        return Err(RuntimeError::TypeError(format!(
            "Date.prototype.{method} called on a non-object"
        )));
    };
    let borrowed = obj_ref.borrow_mut();
    if borrowed.kind() != crate::vm::property::ObjectKind::Date {
        return Err(RuntimeError::TypeError(format!(
            "Date.prototype.{method} called on an incompatible receiver"
        )));
    }
    Ok(RefMut::map(borrowed, |obj| {
        obj.as_any_mut()
            .downcast_mut::<DateObject>()
            .expect("ObjectKind::Date implies DateObject")
    }))
}

fn date_value(this: &Value, method: &str) -> Result<f64, RuntimeError> {
    Ok(as_date(this, method)?.time_value)
}

// ── time ↔ components ──────────────────────────────────────────────────────

/// The local (or UTC, when `utc`) calendar fields of a time value, in the order
/// JS getters want them: year, month (0-11), day, weekday (0 = Sunday),
/// hours, minutes, seconds, milliseconds.
fn parts(ms: i64, utc: bool) -> Option<(i32, u32, u32, u32, u32, u32, u32, u32)> {
    let utc_dt = Utc.timestamp_millis_opt(ms).single()?;
    if utc {
        Some((
            utc_dt.year(),
            utc_dt.month() - 1,
            utc_dt.day(),
            utc_dt.weekday().num_days_from_sunday(),
            utc_dt.hour(),
            utc_dt.minute(),
            utc_dt.second(),
            utc_dt.timestamp_subsec_millis(),
        ))
    } else {
        let local = utc_dt.with_timezone(&Local);
        Some((
            local.year(),
            local.month() - 1,
            local.day(),
            local.weekday().num_days_from_sunday(),
            local.hour(),
            local.minute(),
            local.second(),
            local.timestamp_subsec_millis(),
        ))
    }
}

pub fn date_component(this: &Value, index: usize, utc: bool) -> Result<Value, RuntimeError> {
    // Dispatch entry: the method name is only used for error messages.
    component(this, "Date.prototype.get*", index, utc)
}

pub fn date_timezone_offset(this: &Value) -> Result<Value, RuntimeError> {
    timezone_offset(this)
}

fn component(this: &Value, method: &str, index: usize, utc: bool) -> Result<Value, RuntimeError> {
    let value = date_value(this, method)?;
    if value.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let ms = value as i64;
    let components = parts(ms, utc).ok_or_else(|| {
        RuntimeError::RangeError("Date: time value out of range".to_string())
    })?;
    let out = match index {
        0 => components.0 as f64,
        1 => components.1 as f64,
        2 => components.2 as f64,
        3 => components.3 as f64,
        4 => components.4 as f64,
        5 => components.5 as f64,
        6 => components.6 as f64,
        _ => components.7 as f64,
    };
    Ok(Value::Number(out))
}

/// `getTimezoneOffset()`: minutes to add to local time to reach UTC — i.e. the
/// negation of the UTC offset (ES 21.4.4.20).
fn timezone_offset(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "getTimezoneOffset")?;
    if value.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let utc_dt = Utc.timestamp_millis_opt(value as i64).single();
    let Some(utc_dt) = utc_dt else {
        return Ok(Value::Number(f64::NAN));
    };
    let local = utc_dt.with_timezone(&Local);
    Ok(Value::Number(-(local.offset().local_minus_utc() as f64) / 60.0))
}

/// A calendar component as a `u32`: anything negative or not a number collapses
/// to 0, which is what lets `new Date(2020, -1, 40)` roll over instead of
/// panicking.
fn component_u32(value: f64) -> u32 {
    if value.is_nan() || value < 0.0 {
        0
    } else {
        value as u32
    }
}

/// Build a time value from calendar components, interpreting them as local time
/// (or UTC). Out-of-range combinations answer `NaN` rather than panicking.
fn time_from_components(args: &[Value], utc: bool) -> f64 {
    let num = |i: usize| -> f64 {
        args.get(i).map(|v| v.to_number()).unwrap_or(f64::NAN)
    };
    let year_f = num(0);
    if year_f.is_nan() {
        return f64::NAN;
    }
    let mut year = year_f as i64;
    // ES 21.4.1.1: a year between 0 and 99 is offset by 1900.
    if (0..100).contains(&year) {
        year += 1900;
    }
    if !(i32::MIN as i64..=i32::MAX as i64).contains(&year) {
        return f64::NAN;
    }
    // Months are 0-based on the way in; every other field passes through.
    let month = component_u32(if num(1).is_nan() { 1.0 } else { num(1) + 1.0 });
    let day = component_u32(if num(2).is_nan() { 1.0 } else { num(2) });
    let hour = component_u32(if num(3).is_nan() { 0.0 } else { num(3) });
    let minute = component_u32(if num(4).is_nan() { 0.0 } else { num(4) });
    let second = component_u32(if num(5).is_nan() { 0.0 } else { num(5) });
    let ms = component_u32(if num(6).is_nan() { 0.0 } else { num(6) });

    // `Utc` and `Local` are different `TimeZone` impls, so the two branches
    // cannot share a binding: collapse to the time value first.
    let millis = if utc {
        Utc.with_ymd_and_hms(year as i32, month, day, hour, minute, second)
            .single()
            .map(|dt| dt.timestamp_millis())
    } else {
        Local.with_ymd_and_hms(year as i32, month, day, hour, minute, second)
            .single()
            .map(|dt| dt.timestamp_millis())
    };
    match millis {
        Some(m) => m as f64 + ms as f64,
        None => f64::NAN,
    }
}

/// `MakeTime`-style parse of the date *string* forms the constructor and
/// `Date.parse` accept (ES 21.4.1.15, restricted to the everyday shapes):
/// RFC 3339 with an offset, a date-time without one (local), and a date-only
/// form (UTC).
fn parse_date_string(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.timestamp_millis() as f64);
    }
    // Date-time without an offset: local time.
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f") {
        if let Some(local) = Local.from_local_datetime(&naive).single() {
            return Some(local.timestamp_millis() as f64);
        }
    }
    // Date-only: UTC midnight (ES 21.4.1.15 step 4).
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        if let Some(naive) = date.and_hms_opt(0, 0, 0) {
            return Some(Utc.from_utc_datetime(&naive).timestamp_millis() as f64);
        }
    }
    // `2020-01-01T00:00` and `2020-01` are accepted by every host in practice.
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M") {
        if let Some(local) = Local.from_local_datetime(&naive).single() {
            return Some(local.timestamp_millis() as f64);
        }
    }
    None
}

/// Turn the constructor's argument list into a `[[DateValue]]`.
fn date_value_from_args(args: &[Value]) -> f64 {
    match args.first() {
        None => Utc::now().timestamp_millis() as f64,
        // One value: a number is a time value, anything else is parsed as a
        // date string (ES 21.4.2.1). A Date object is ToPrimitive'd by the VM
        // before it reaches here; a Number object arrives as a number.
        Some(first) if args.len() == 1 => match first {
            Value::Object(obj) if obj.borrow().kind() == crate::vm::property::ObjectKind::Date => {
                obj.borrow()
                    .as_any()
                    .downcast_ref::<DateObject>()
                    .map(|d| d.time_value)
                    .unwrap_or(f64::NAN)
            }
            _ => {
                let n = first.to_number();
                if n.is_nan() {
                    match parse_date_string(&first.to_js_string()) {
                        Some(v) => v,
                        None => f64::NAN,
                    }
                } else {
                    n
                }
            }
        },
        // Two or more: calendar components.
        _ => time_from_components(args, false),
    }
}

// ── constructor & statics ──────────────────────────────────────────────────

/// `new Date(...)` — builds the object. `Date()` *without* `new` returns a
/// string, so it cannot share this path (the VM routes `new` here).
pub fn date_construct_value(args: &[Value], proto: Option<Value>) -> Result<Value, RuntimeError> {
    let time_value = date_value_from_args(args);
    let proto = match proto {
        Some(Value::Object(p)) => Some(p),
        _ => None,
    };
    Ok(Value::Object(Rc::new(RefCell::new(DateObject::new(
        time_value, proto,
    )))))
}

/// `Date()` called as a function: same string as `new Date().toString()`.
pub fn date_as_function(_args: &[Value]) -> Result<Value, RuntimeError> {
    let now = Value::Object(Rc::new(RefCell::new(DateObject::new(
        Utc::now().timestamp_millis() as f64,
        None,
    ))));
    date_to_string(&now, &[])
}

pub fn date_now(_args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Number(Utc::now().timestamp_millis() as f64))
}

pub fn date_parse(args: &[Value]) -> Result<Value, RuntimeError> {
    let text = args.first().map(|v| v.to_js_string()).unwrap_or_default();
    Ok(Value::Number(
        parse_date_string(&text).unwrap_or(f64::NAN),
    ))
}

pub fn date_utc(args: &[Value]) -> Result<Value, RuntimeError> {
    Ok(Value::Number(time_from_components(args, true)))
}

// ── prototype registration ─────────────────────────────────────────────────

pub fn register_date_prototype(proto: &Rc<RefCell<dyn JSObject>>) {
    use super::set_prototype_method;
    // Conversions
    set_prototype_method(proto, "getTime", |this, _| component(this, "getTime", 8, false));
    set_prototype_method(proto, "valueOf", |this, _| date_value_of(this));
    set_prototype_method(proto, "toISOString", |this, _| date_to_iso_string(this));
    set_prototype_method(proto, "toJSON", |this, _| date_to_json(this));
    set_prototype_method(proto, "toString", |this, _| date_to_string(this, &[]));
    set_prototype_method(proto, "toDateString", |this, _| date_to_date_string(this));
    set_prototype_method(proto, "toTimeString", |this, _| date_to_time_string(this));
    set_prototype_method(proto, "toUTCString", |this, _| date_to_utc_string(this));
    set_prototype_method(proto, "toLocaleString", |this, _| date_to_string(this, &[]));
    set_prototype_method(proto, "toLocaleDateString", |this, _| date_to_date_string(this));
    set_prototype_method(proto, "toLocaleTimeString", |this, _| date_to_time_string(this));
    // Local components
    set_prototype_method(proto, "getFullYear", |this, _| component(this, "getFullYear", 0, false));
    set_prototype_method(proto, "getMonth", |this, _| component(this, "getMonth", 1, false));
    set_prototype_method(proto, "getDate", |this, _| component(this, "getDate", 2, false));
    set_prototype_method(proto, "getDay", |this, _| component(this, "getDay", 3, false));
    set_prototype_method(proto, "getHours", |this, _| component(this, "getHours", 4, false));
    set_prototype_method(proto, "getMinutes", |this, _| component(this, "getMinutes", 5, false));
    set_prototype_method(proto, "getSeconds", |this, _| component(this, "getSeconds", 6, false));
    set_prototype_method(proto, "getMilliseconds", |this, _| {
        component(this, "getMilliseconds", 7, false)
    });
    set_prototype_method(proto, "getTimezoneOffset", |this, _| timezone_offset(this));
    // UTC components
    set_prototype_method(proto, "getUTCFullYear", |this, _| component(this, "getUTCFullYear", 0, true));
    set_prototype_method(proto, "getUTCMonth", |this, _| component(this, "getUTCMonth", 1, true));
    set_prototype_method(proto, "getUTCDate", |this, _| component(this, "getUTCDate", 2, true));
    set_prototype_method(proto, "getUTCDay", |this, _| component(this, "getUTCDay", 3, true));
    set_prototype_method(proto, "getUTCHours", |this, _| component(this, "getUTCHours", 4, true));
    set_prototype_method(proto, "getUTCMinutes", |this, _| component(this, "getUTCMinutes", 5, true));
    set_prototype_method(proto, "getUTCSeconds", |this, _| component(this, "getUTCSeconds", 6, true));
    // Setters
    set_prototype_method(proto, "setTime", |this, args| date_set_time(this, args));
    set_prototype_method(proto, "setFullYear", |this, args| date_set_full_year(this, args));
    set_prototype_method(proto, "setMonth", |this, args| date_set_month(this, args));
    set_prototype_method(proto, "setDate", |this, args| date_set_date(this, args));
}

pub fn register_date_statics(ctor: &Value) {
    let Value::Object(obj_ref) = ctor else {
        return;
    };
    for name in ["now", "parse", "UTC"] {
        let _ = obj_ref.borrow_mut().define_property(
            PropertyKey::from_str(name),
            PropertyDescriptor {
                value: Value::Object(Rc::new(RefCell::new(NativeFunctionObject::new(
                    &format!("Date.{name}"),
                )))),
                writable: true,
                enumerable: false,
                configurable: true,
                getter: None,
                setter: None,
            },
        );
    }
}

// ── instance behaviour ─────────────────────────────────────────────────────

pub fn date_value_of(this: &Value) -> Result<Value, RuntimeError> {
    Ok(Value::Number(date_value(this, "valueOf")?))
}

pub fn date_to_iso_string(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toISOString")?;
    if value.is_nan() {
        return Err(RuntimeError::RangeError(
            "Invalid time value".to_string(),
        ));
    }
    let dt = Utc.timestamp_millis_opt(value as i64).single();
    let Some(dt) = dt else {
        return Err(RuntimeError::RangeError("Invalid time value".to_string()));
    };
    Ok(Value::string(&dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()))
}

/// `toJSON` answers `null` for an invalid date instead of throwing (ES
/// 21.4.4.35 step 4) — that is what makes `JSON.stringify(new Date('x'))` work.
pub fn date_to_json(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toJSON")?;
    if value.is_nan() {
        return Ok(Value::Null);
    }
    date_to_iso_string(this)
}

/// `Date.prototype.toString`: `Mon Sep 28 2026 12:00:00 GMT+0800`. The trailing
/// timezone *name* node prints (`(China Standard Time)`) needs a locale
/// database this engine does not carry.
pub fn date_to_string(this: &Value, _args: &[Value]) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toString")?;
    if value.is_nan() {
        return Ok(Value::string("Invalid Date"));
    }
    let Some(utc_dt) = Utc.timestamp_millis_opt(value as i64).single() else {
        return Ok(Value::string("Invalid Date"));
    };
    let local = utc_dt.with_timezone(&Local);
    Ok(Value::string(
        &local.format("%a %b %d %Y %H:%M:%S GMT%z").to_string(),
    ))
}

pub fn date_to_date_string(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toDateString")?;
    if value.is_nan() {
        return Ok(Value::string("Invalid Date"));
    }
    let Some(utc_dt) = Utc.timestamp_millis_opt(value as i64).single() else {
        return Ok(Value::string("Invalid Date"));
    };
    Ok(Value::string(
        &utc_dt
            .with_timezone(&Local)
            .format("%a %b %d %Y")
            .to_string(),
    ))
}

pub fn date_to_time_string(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toTimeString")?;
    if value.is_nan() {
        return Ok(Value::string("Invalid Date"));
    }
    let Some(utc_dt) = Utc.timestamp_millis_opt(value as i64).single() else {
        return Ok(Value::string("Invalid Date"));
    };
    Ok(Value::string(
        &utc_dt
            .with_timezone(&Local)
            .format("%H:%M:%S GMT%z")
            .to_string(),
    ))
}

pub fn date_to_utc_string(this: &Value) -> Result<Value, RuntimeError> {
    let value = date_value(this, "toUTCString")?;
    if value.is_nan() {
        return Ok(Value::string("Invalid Date"));
    }
    let Some(utc_dt) = Utc.timestamp_millis_opt(value as i64).single() else {
        return Ok(Value::string("Invalid Date"));
    };
    Ok(Value::string(
        &utc_dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
    ))
}

pub fn date_set_time(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let time = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
    let mut date = as_date(this, "setTime")?;
    date.time_value = time;
    Ok(Value::Number(time))
}

/// `setFullYear(year[, month[, date]])` — the other fields are preserved.
pub fn date_set_full_year(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut date = as_date(this, "setFullYear")?;
    let current = date.time_value;
    if current.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let (_, month, day, _, hour, min, sec, ms) =
        parts(current as i64, false).unwrap_or((1970, 0, 1, 0, 0, 0, 0, 0));
    let year = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
    let month = match args.get(1) {
        Some(v) => v.to_number() as i64,
        None => month as i64,
    };
    let day = match args.get(2) {
        Some(v) => v.to_number() as i64,
        None => day as i64,
    };
    let new = time_from_components(
        &[
            Value::Number(year),
            Value::Number(month as f64),
            Value::Number(day as f64),
            Value::Number(hour as f64),
            Value::Number(min as f64),
            Value::Number(sec as f64),
            Value::Number(ms as f64),
        ],
        false,
    );
    date.time_value = new;
    Ok(Value::Number(new))
}

pub fn date_set_month(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    set_one_component(this, "setMonth", args, 1)
}

pub fn date_set_date(this: &Value, args: &[Value]) -> Result<Value, RuntimeError> {
    set_one_component(this, "setDate", args, 2)
}

/// Replace one calendar component, keeping the rest (an out-of-range value rolls
/// over the way ES specifies: `setMonth(12)` is January of the next year).
fn set_one_component(
    this: &Value,
    method: &str,
    args: &[Value],
    which: usize,
) -> Result<Value, RuntimeError> {
    let mut date = as_date(this, method)?;
    let current = date.time_value;
    if current.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let parts_now = parts(current as i64, false).unwrap_or((1970, 0, 1, 0, 0, 0, 0, 0));
    let new_value = args.first().map(|v| v.to_number()).unwrap_or(f64::NAN);
    let mut components = [
        Value::Number(parts_now.0 as f64),
        Value::Number(parts_now.1 as f64),
        Value::Number(parts_now.2 as f64),
    ];
    if which == 1 {
        // `setMonth(month[, date])`
        components[1] = Value::Number(new_value);
        if let Some(d) = args.get(1) {
            components[2] = Value::Number(d.to_number());
        }
    } else {
        components[2] = Value::Number(new_value);
    }
    let new = time_from_components(
        &[
            components[0].clone(),
            components[1].clone(),
            components[2].clone(),
            Value::Number(parts_now.4 as f64),
            Value::Number(parts_now.5 as f64),
            Value::Number(parts_now.6 as f64),
            Value::Number(parts_now.7 as f64),
        ],
        false,
    );
    date.time_value = new;
    Ok(Value::Number(new))
}
