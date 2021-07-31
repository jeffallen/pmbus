use anyhow::{bail, Result};

use convert_case::{Case, Casing};
use ron::de::from_reader;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fmt::Write;
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct High(u8);

#[derive(Debug, Deserialize)]
struct Low(u8);

#[derive(Debug, Deserialize)]
struct Value(u16, String);

#[derive(Debug, Deserialize)]
enum Values<T> {
    Scalar,
    Sentinels(T),
}

#[derive(Debug, Deserialize)]
enum Bits {
    Bitrange(High, Low),
    Bit(u8),
}

#[derive(Debug, Deserialize)]
struct Field {
    name: String,
    bits: Bits,
    values: Values<HashMap<String, Value>>,
}

#[derive(Clone, Debug, Deserialize)]
enum Operation {
    ReadByte,
    WriteByte,
    SendByte,
    ReadWord,
    WriteWord,
    ReadWord32,
    ReadBlock,
    WriteBlock,
    ProcessCall,
    MfrDefined,
    Extended,
    Illegal,
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
struct Command(u8, String, Operation, Operation);

#[derive(Debug, Deserialize)]
struct Commands(Vec<Command>, HashMap<String, HashMap<String, Field>>);

fn reg_sizes(cmds: &Vec<Command>) -> Result<HashMap<String, Option<usize>>> {
    let mut sizes = HashMap::new();

    for cmd in cmds {
        let size = match cmd.3 {
            Operation::ReadByte => Some(1),
            Operation::ReadWord => Some(2),
            Operation::ReadWord32 => Some(4),
            Operation::WriteByte
            | Operation::WriteWord
            | Operation::WriteBlock => {
                bail!("illegal read operation {:?} on {}", cmd.3, cmd.1);
            }
            _ => None,
        };

        sizes.insert(cmd.1.clone(), size);
    }

    Ok(sizes)
}

#[rustfmt::skip::macros(writeln)]
fn output_commands(cmds: &Commands) -> Result<String> {
    let mut s = String::new();

    writeln!(&mut s, r##"
pub use num_derive::{{FromPrimitive, ToPrimitive}};
pub use num_traits::{{FromPrimitive, ToPrimitive}};

#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, PartialEq, FromPrimitive)]
#[repr(u8)]
pub enum CommandCode {{"##)?;

    for cmd in &cmds.0 {
        writeln!(&mut s, "    {} = 0x{:x},", cmd.1, cmd.0)?;
    }

    writeln!(&mut s, r##"}}

impl CommandCode {{
    pub fn read_op(&self) -> Operation {{
        match self {{"##)?;

    for cmd in &cmds.0 {
        writeln!(&mut s,
            "            CommandCode::{} => Operation::{:?},", cmd.1, cmd.3)?;
    }

    writeln!(&mut s, "        }}\n    }}")?;

    writeln!(&mut s, r##"
    pub fn write_op(&self) -> Operation {{
        match self {{"##)?;

    for cmd in &cmds.0 {
        writeln!(&mut s,
            "            CommandCode::{} => Operation::{:?},", cmd.1, cmd.2)?;
    }

    writeln!(&mut s, "        }}\n    }}")?;

    writeln!(&mut s, r##"
    pub fn data(
        &self,
        payload: &[u8],
        iter: impl Fn(&dyn Field, &dyn Value)
    ) -> Result<(), Error> {{
        match self {{"##)?;

    for cmd in &cmds.0 {
        if cmds.1.get(&cmd.1).is_none() {
            continue;
        }

        writeln!(&mut s, r##"            CommandCode::{} => {{
                use {}::CommandData;
                if let Some(data) = CommandData::from_slice(payload) {{
                    data.fields(iter);
                    Ok(())
                }} else {{
                    Err(Error::ShortData)
                }}
            }}"##, cmd.1, cmd.1)?;
    }

    writeln!(&mut s, "            _ => Ok(()),")?;
    writeln!(&mut s, "        }}\n    }}\n}}")?;

    Ok(s)
}

fn bitrange(bits: &Bits) -> (u8, u8) {
    match bits {
        Bits::Bit(pos) => (*pos, *pos),
        Bits::Bitrange(High(high), Low(low)) => (*high, *low),
    }
}

#[rustfmt::skip::macros(bail)]
fn validate(
    cmd: &str,
    fields: &HashMap<String, Field>,
    sizes: &HashMap<String, Option<usize>>,
) -> Result<()> {
    let size = match sizes.get(cmd) {
        Some(Some(size)) => size,
        Some(None) => {
            bail!("command {} does not allow a register", cmd);
        }
        None => {
            bail!("command {} does not exist", cmd);
        }
    };

    let mut v: Vec<Option<&String>> = vec![None; *size * 8];

    for (f, field) in fields {
        let (high, low) = bitrange(&field.bits);

        if high < low {
            bail!("{}: field \"{}\" has illegal bit range", cmd, f);
        }

        if high as usize >= size * 8 {
            bail!("{}: field \"{}\" has high bit that exceeds size", cmd, f);
        }

        for bit in low..=high {
            match v[bit as usize] {
                None => {
                    v[bit as usize] = Some(f);
                }
                Some(o) => {
                    bail!(
                        "{}: field \"{}\" overlaps with \"{}\" at bit {}",
                        cmd, f, o, bit
                    );
                }
            }
        }
    }

    Ok(())
}

#[rustfmt::skip::macros(writeln)]
fn output_scalar(name: &str, width: usize) -> Result<String> {
    let mut s = String::new();
    let bits = ((width + 8) / 8) * 8;

    writeln!(&mut s, r##"
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive)]
    #[allow(non_camel_case_types)]
    pub struct {}(pub u{});

    impl {} {{
        fn desc(&self) -> &'static str {{
            "(scalar value)"
        }}
    }}"##, name, bits, name)?;

    Ok(s)
}

#[rustfmt::skip::macros(writeln)]
fn output_value(
    name: &str,
    values: &Values<HashMap<String, Value>>,
    width: usize,
) -> Result<String> {
    let mut s = String::new();

    let values = match values {
        Values::Sentinels(ref v) => v,
        Values::Scalar => {
            return output_scalar(name, width);
        }
    };

    writeln!(&mut s, r##"
    #[derive(Copy, Clone, Debug, PartialEq, FromPrimitive, ToPrimitive)]
    #[allow(non_camel_case_types)]
    pub enum {} {{"##, name)?;

    for (v, value) in values {
        writeln!(
            &mut s, "        {} = 0b{:0width$b},",
            v, value.0, width = width
        )?;
    }

    writeln!(&mut s, "    }}")?;

    writeln!(&mut s, r##"
    impl {} {{
        fn desc(&self) -> &'static str {{
            match self {{"##, name)?;

    for (v, value) in values {
        writeln!(
            &mut s, "                {}::{} => \"{}\",",
            name, v, value.1
        )?;
    }

    writeln!(&mut s, "            }}\n        }}\n     }}")?;
    Ok(s)
}

#[rustfmt::skip::macros(writeln)]
fn output_databytes(
    cmd: &str,
    fields: &HashMap<String, Field>,
    sizes: &HashMap<String, Option<usize>>,
) -> Result<String> {
    let mut s = String::new();
    let size = sizes.get(cmd).unwrap().unwrap();
    let bits = size * 8;

    writeln!(&mut s, r##"
#[allow(non_snake_case)]
pub mod {} {{
    use super::Bitpos;
    use super::Bitwidth;
    use num_derive::FromPrimitive;
    use num_derive::ToPrimitive;

    use num_traits::FromPrimitive;
    use num_traits::ToPrimitive;

    pub struct CommandData(pub u{});

    #[derive(Copy, Clone, Debug, PartialEq)]
    pub enum Field {{"##, cmd, bits)?;

    for f in fields.keys() {
        writeln!(&mut s, "        {},", f)?;
    }

    writeln!(&mut s, r##"    }}

    impl core::fmt::Display for Field {{
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{
            use super::Field;
            write!(f, "{{}}", self.name())
        }}
    }}

    impl super::Field for Field {{
        fn bits(&self) -> (Bitpos, Bitwidth) {{
            match self {{"##)?;

    for (f, field) in fields {
        let (high, low) = bitrange(&field.bits);

        let pos = low;
        let width = high - low + 1;

        writeln!(&mut s, "                Field::{} => \
            (Bitpos({}), Bitwidth({})),", f, pos, width)?;
    }

    writeln!(&mut s, "            }}\n        }}")?;

    writeln!(&mut s, r##"
        fn name(&self) -> &'static str {{
            match self {{"##)?;

    for (f, field) in fields {
        writeln!(&mut s, "                Field::{} => \"{}\",", f, field.name)?;
    }

    writeln!(&mut s, "            }}\n        }}\n    }}")?;

    for (f, field) in fields {
        let (high, low) = bitrange(&field.bits);
        let width = high - low + 1;
        write!(&mut s, "{}", output_value(&f, &field.values, width.into())?)?;
    }

    writeln!(&mut s, r##"
    #[derive(Copy, Clone, Debug, PartialEq)]
    pub enum Value {{"##)?;

    for (f, _) in fields {
        writeln!(&mut s, "        {}({}),", f, f)?;
    }

    writeln!(&mut s, "        Unknown(u{}),\n    }}", bits)?;

    writeln!(&mut s, r##"
    impl super::Value for Value {{
        fn desc(&self) -> &'static str {{
            match self {{"##)?;

    for (f, _) in fields {
        writeln!(&mut s, "                Value::{}(v) => v.desc(),", f)?;
    }

    writeln!(&mut s, "                Value::Unknown(_) => \"<unknown>\",")?;
    writeln!(&mut s, "            }}\n        }}")?;

    writeln!(&mut s, r##"
        fn scalar(&self) -> bool {{
            match self {{"##)?;

    for (f, field) in fields {
        if let Values::Scalar = &field.values {
            writeln!(&mut s, "                Value::{}(_) => true,", f)?;
        }
    }

    writeln!(&mut s, "                _ => false,")?;
    writeln!(&mut s, "            }}\n        }}")?;

    writeln!(&mut s, r##"
        fn raw(&self) -> u32 {{
            match self {{"##)?;

    for (f, field) in fields {
        match field.values {
            Values::Sentinels(_) => {
                writeln!(
                    &mut s,
                    "                Value::{}(v) => v.to_u32().unwrap(),",
                    f
                )?;
            }
            Values::Scalar => {
                writeln!(
                    &mut s,
                    "                Value::{}(v) => v.0 as u32,",
                    f
                )?;
            }
        }
    }

    writeln!(&mut s, "                Value::Unknown(v) => *v as u32,")?;
    writeln!(&mut s, "            }}\n        }}\n    }}")?;

    writeln!(&mut s, r##"
    impl core::fmt::Display for Value {{
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {{
            if super::Value::scalar(self) {{
                write!(
                    f, "0x{{:x}} ({{}})",
                    super::Value::raw(self), super::Value::raw(self)
                )
            }} else {{
                write!(
                    f, "0b{{:b}} ({{}})",
                    super::Value::raw(self), super::Value::desc(self)
                )
            }}
        }}
    }}"##)?;

    writeln!(&mut s, r##"
    impl CommandData {{
        pub fn from_slice(slice: &[u8]) -> Option<Self> {{
            use core::convert::TryInto;

            let v: Result<&[u8; {}], _> = slice.try_into();

            match v {{
                Ok(v) => Some(Self(u{}::from_le_bytes(*v))),
                Err(_) => None,
            }}
        }}

        pub fn field(bit: Bitpos) -> Option<(Field, Bitwidth)> {{
            match bit.0 {{"##, size, bits)?;

    for (f, field) in fields {
        let (high, low) = bitrange(&field.bits);

        writeln!(&mut s,
            "                {} => Some((Field::{}, Bitwidth({}))),",
            low, f, high - low + 1
        )?;
    }

    writeln!(&mut s, "                _ => None,")?;
    writeln!(&mut s, "            }}\n        }}")?;

    writeln!(&mut s, r##"
        pub fn get_val(&self, field: Field) -> u{} {{
            use super::Field;
            let (pos, width) = field.bits();
            (self.0 >> pos.0) & ((1 << width.0) - 1)
        }}
        
        pub fn get(&self, field: Field) -> Value {{
            let raw = self.get_val(field);

            match field {{"##, bits)?;

    for (f, _) in fields {
        writeln!(&mut s, r##"
                Field::{} => {{
                    match {}::from_u{}(raw) {{
                        Some(t) => Value::{}(t),
                        None => Value::Unknown(raw),
                    }}
                }}"##, f, f, bits, f)?;
    }

    writeln!(&mut s, "            }}\n        }}")?;

    writeln!(&mut s, r##"
        fn set_val(&mut self, field: Field, raw: u{}) {{
            use super::Field;
            let (pos, width) = field.bits();
            self.0 &= !(((1 << width.0) - 1) << pos.0);
            self.0 |= raw << pos.0;
        }}"##, bits)?;

    for (f, _) in fields {
        let method = f.from_case(Case::Camel).to_case(Case::Snake);

        writeln!(&mut s, r##"
        pub fn get_{}(&mut self) -> Option<{}> {{
            match self.get(Field::{}) {{
                Value::{}(v) => Some(v),
                _ => None,
            }}
        }}"##, method, f, f, f)?;

        writeln!(&mut s, r##"
        pub fn set_{}(&mut self, val: {}) {{
            self.set_val(Field::{}, val.to_u{}().unwrap());
        }}"##, method, f, f, bits)?;
    }

    writeln!(&mut s, "    }}")?;

    writeln!(&mut s, r##"
    impl super::CommandData for CommandData {{
        fn fields(
            &self,
            mut iter: impl FnMut(&dyn super::Field, &dyn super::Value)
        ) {{
            let mut pos = 0;

            while pos < {} {{
                if let Some((field, width)) = CommandData::field(Bitpos(pos)) {{
                    let val = self.get(field);
                    iter(&field, &val);
                    pos += width.0;
                }} else {{
                    pos += 1;
                }}
            }}
        }}

        fn raw(&self) -> (u32, Bitwidth) {{
            (self.0 as u32, Bitwidth({}))
        }}
    }}"##, bits, bits)?;

    writeln!(&mut s, "}}")?;

    Ok(s)
}

#[rustfmt::skip::macros(writeln)]
fn output_device(device: &str) -> Result<String> {
    let mut s = String::new();

    writeln!(&mut s, r##"
pub mod {} {{
    pub use super::CommandData;
    pub use super::Value;
    pub use super::Field;
    pub use super::Bitwidth;
    pub use super::Bitpos;
    pub use super::Operation;
    pub use super::Error;

    include!(concat!(env!("OUT_DIR"), "/{}.rs"));
}}"##, device, device)?;

    Ok(s)
}

fn open_file(filename: &str) -> Result<File> {
    let mut dir = PathBuf::from(&env::var("CARGO_MANIFEST_DIR").unwrap());
    dir.push("src");
    dir.push(filename);

    match File::open(dir) {
        Ok(f) => {
            println!("cargo:rerun-if-changed=src/{}", filename);
            Ok(f)
        }
        Err(e) => {
            bail!("failed to open {}: {}", filename, e);
        }
    }
}

fn codegen() -> Result<()> {
    use std::io::Write;

    //
    // First, consume our common commands.
    //
    let mut dir = PathBuf::from(&env::var("CARGO_MANIFEST_DIR")?);
    dir.push("src");

    let f = open_file("commands.ron")?;

    let cmds: Commands = match from_reader(f) {
        Ok(cmds) => cmds,
        Err(e) => {
            bail!("failed to parse commands.ron: {}", e);
        }
    };

    let sizes = reg_sizes(&cmds.0)?;
    let dbs = &cmds.1;

    let out_dir = env::var("OUT_DIR")?;
    let dest_path = Path::new(&out_dir).join("commands.rs");
    let mut file = File::create(&dest_path)?;

    let out = output_commands(&cmds)?;
    file.write_all(out.as_bytes())?;

    for (cmd, fields) in dbs {
        validate(cmd, fields, &sizes)?;
        let out = output_databytes(cmd, fields, &sizes)?;
        file.write_all(out.as_bytes())?;
    }

    let devices = ["adm1272", "tps546b24a"];

    let dest_path = Path::new(&out_dir).join("devices.rs");
    let mut dfile = File::create(&dest_path)?;

    //
    // Now we need to iterate over our devices.  For each one, we'll generate
    // our flattened module, and then include it in our flattened file of
    // all devices.
    //
    for dev in &devices {
        let dest_path = Path::new(&out_dir).join(format!("{}.rs", dev));
        let mut file = File::create(&dest_path)?;

        let fname = format!("{}.ron", dev);
        let f = open_file(&fname)?;

        let mut dcmds: Commands = match from_reader(f) {
            Ok(dcmds) => dcmds,
            Err(e) => {
                bail!("failed to parse {}: {}", fname, e);
            }
        };

        //
        // Flatten our commands and output them
        //
        let mut h: HashSet<u8> = HashSet::new();

        for cmd in &dcmds.0 {
            h.insert(cmd.0);
        }

        for cmd in &cmds.0 {
            if h.get(&cmd.0).is_none() {
                dcmds.0.push(cmd.clone());
            }
        }

        let out = output_commands(&dcmds)?;
        file.write_all(out.as_bytes())?;

        let dsizes = reg_sizes(&dcmds.0)?;

        for (cmd, fields) in &dcmds.1 {
            validate(&cmd, &fields, &dsizes)?;
        }

        //
        // Now emit data payloads, allowing the device definition to
        // override any common payload.
        //
        for (cmd, fields) in dbs {
            if let Some(fields) = dcmds.1.get(cmd) {
                let out = output_databytes(cmd, fields, &dsizes)?;
                file.write_all(out.as_bytes())?;
                dcmds.1.remove(cmd);
            } else {
                let out = output_databytes(cmd, fields, &sizes)?;
                file.write_all(out.as_bytes())?;
            }
        }

        for (cmd, fields) in &dcmds.1 {
            let out = output_databytes(cmd, fields, &dsizes)?;
            file.write_all(out.as_bytes())?;
        }

        let out = output_device(dev)?;
        dfile.write_all(out.as_bytes())?;
    }

    Ok(())
}

fn main() {
    if let Err(e) = codegen() {
        println!("code generation failed: {}", e);
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=build.rs");
}
