//! Boot image: bytecode data (see "bytecode as data" in prims.rs) as a flat tagged binary.
//! `neant --build-boot` writes src/neant/image.nb with the neant compiler; main.rs embeds it with include_bytes!.
use crate::value::*;
use std::sync::Arc;
use Value::*;

pub fn dump(v: &Value) -> R<Vec<u8>> { let mut out = Vec::new(); put(v, &mut out)?; Ok(out) }

pub fn load(b: &[u8]) -> R<Value> {
    if b.is_empty() { return err("image: empty"); }
    let mut p = 0;
    let v = get(b, &mut p)?;
    if p != b.len() { return err("image: trailing bytes"); }
    Ok(v)
}

fn put_len(o: &mut Vec<u8>, n: usize) { o.extend_from_slice(&(n as u32).to_le_bytes()); }
fn put_str(o: &mut Vec<u8>, s: &str) { put_len(o, s.len()); o.extend_from_slice(s.as_bytes()); }

fn put(v: &Value, o: &mut Vec<u8>) -> R<()> {
    match v {
        Null => o.push(0),
        Bool(b) => { o.push(1); o.push(*b as u8); }
        Int(i) => { o.push(2); o.extend_from_slice(&i.to_le_bytes()); }
        Float(f) => { o.push(3); o.extend_from_slice(&f.to_le_bytes()); }
        Char(c) => { o.push(4); o.extend_from_slice(&(*c as u32).to_le_bytes()); }
        Symbol(s) => { o.push(5); put_str(o, s); }
        Bools(v) => { o.push(6); put_len(o, v.len()); o.extend(v.iter().map(|&b| b as u8)); }
        Ints(v) => { o.push(7); put_len(o, v.len()); for i in v.iter() { o.extend_from_slice(&i.to_le_bytes()); } }
        Floats(v) => { o.push(8); put_len(o, v.len()); for f in v.iter() { o.extend_from_slice(&f.to_le_bytes()); } }
        Chars(v) => { o.push(9); put_str(o, &v.iter().collect::<String>()); }
        Syms(v) => { o.push(10); put_len(o, v.len()); for s in v.iter() { put_str(o, s); } }
        List(v) => { o.push(11); put_len(o, v.len()); for x in v.iter() { put(x, o)?; } }
        Dict(d) => { o.push(12); put(&d.keys, o)?; put(&d.vals, o)?; }
        Date(d) => { o.push(13); o.extend_from_slice(&d.to_le_bytes()); }
        Time(t) => { o.push(14); o.extend_from_slice(&t.to_le_bytes()); }
        Dates(v) => { o.push(15); put_len(o, v.len()); for d in v.iter() { o.extend_from_slice(&d.to_le_bytes()); } }
        Times(v) => { o.push(16); put_len(o, v.len()); for t in v.iter() { o.extend_from_slice(&t.to_le_bytes()); } }
        Byte(b) => { o.push(17); o.push(*b); }
        Bytes(v) => { o.push(18); put_len(o, v.len()); o.extend_from_slice(v); }
        Lambda(_) | Prim(_) | Adv(..) | Proj(..) | Closure(..) => return err("image: functions are not serializable"),
        Shared(_) | Thread(_) => return err("image: shared cells and thread handles are not serializable"),
    }
    Ok(())
}

fn take<'a>(b: &'a [u8], p: &mut usize, n: usize) -> R<&'a [u8]> {
    let s = b.get(*p..*p + n).ok_or_else(|| NError("image: truncated".into()))?;
    *p += n;
    Ok(s)
}
fn get_len(b: &[u8], p: &mut usize) -> R<usize> { Ok(u32::from_le_bytes(take(b, p, 4)?.try_into().unwrap()) as usize) }
fn get_i64(b: &[u8], p: &mut usize) -> R<i64> { Ok(i64::from_le_bytes(take(b, p, 8)?.try_into().unwrap())) }
fn get_f64(b: &[u8], p: &mut usize) -> R<f64> { Ok(f64::from_le_bytes(take(b, p, 8)?.try_into().unwrap())) }
fn get_str(b: &[u8], p: &mut usize) -> R<String> {
    let n = get_len(b, p)?;
    String::from_utf8(take(b, p, n)?.to_vec()).map_err(|_| NError("image: bad utf8".into()))
}

fn get(b: &[u8], p: &mut usize) -> R<Value> {
    let tag = take(b, p, 1)?[0];
    Ok(match tag {
        0 => Null,
        1 => Bool(take(b, p, 1)?[0] != 0),
        2 => Int(get_i64(b, p)?),
        3 => Float(get_f64(b, p)?),
        4 => Char(char::from_u32(get_len(b, p)? as u32).unwrap_or('?')),
        5 => Symbol(Arc::from(get_str(b, p)?)),
        6 => { let n = get_len(b, p)?; bools(take(b, p, n)?.iter().map(|&x| x != 0).collect()) }
        7 => { let n = get_len(b, p)?; ints((0..n).map(|_| get_i64(b, p)).collect::<R<_>>()?) }
        8 => { let n = get_len(b, p)?; floats((0..n).map(|_| get_f64(b, p)).collect::<R<_>>()?) }
        9 => chars(get_str(b, p)?.chars().collect()),
        10 => { let n = get_len(b, p)?; syms((0..n).map(|_| get_str(b, p).map(Arc::from)).collect::<R<_>>()?) }
        11 => { let n = get_len(b, p)?; list((0..n).map(|_| get(b, p)).collect::<R<_>>()?) }
        12 => { let keys = get(b, p)?; let vals = get(b, p)?; Dict(Arc::new(crate::value::Dict { keys, vals })) }
        13 => Date(i32::from_le_bytes(take(b, p, 4)?.try_into().unwrap())),
        14 => Time(get_i64(b, p)?),
        15 => { let n = get_len(b, p)?; dates((0..n).map(|_| take(b, p, 4).map(|s| i32::from_le_bytes(s.try_into().unwrap()))).collect::<R<_>>()?) }
        16 => { let n = get_len(b, p)?; times((0..n).map(|_| get_i64(b, p)).collect::<R<_>>()?) }
        17 => Byte(take(b, p, 1)?[0]),
        18 => { let n = get_len(b, p)?; bytes(take(b, p, n)?.to_vec()) }
        t => return err(format!("image: unknown tag {t}")),
    })
}
