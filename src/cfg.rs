use {ErrorKind, Error};
use std::str::from_utf8;
use nom::IResult;
use std::io::Write;

#[derive(Debug, Clone, Copy)]
pub enum CfgOp {
    All,
    Any,
}

#[derive(Debug)]
pub enum Cfg {
    Op {
        op: CfgOp,
        operands: Vec<Cfg>
    },
    Not(Vec<Cfg>),
    Cfg(String),
    Equal(String, String)
}

pub fn parse_target(target: &str) -> Result<Vec<Cfg>, Error> {
    debug!("{:?}", target);
    match cfg(target.as_bytes()) {
        IResult::Done(i, o) => {
            debug!("i = {:?}", i);
            Ok(o)
        }
        IResult::Error(e) => panic!("error {:?}", e),
        IResult::Incomplete(n) => panic!("incomplete {:?}", n)
    }
}

pub fn to_nix(w: &mut Write, target: &[Cfg]) -> Result<(), Error> {
    to_nix_op(w, CfgOp::Any, target)
}


fn to_nix_op(w: &mut Write, op: CfgOp, target: &[Cfg]) -> Result<(), Error> {
    let mut is_first = true;
    for cfg in target {
        if !is_first {
            match op {
                CfgOp::All => write!(w, " && ")?,
                CfgOp::Any => write!(w, " || ")?,
            }
        }
        is_first = false;
        match *cfg {
            Cfg::Op { op, ref operands } => to_nix_op(w, op, operands)?,
            Cfg::Not(ref cfg) => {
                write!(w, "!(")?;
                to_nix_op(w, CfgOp::All, cfg)?;
                write!(w, ")")?;
            }
            Cfg::Equal(ref key, ref value) => {
                match key.as_str() {
                    "target_os" => cfg_value(w, "kernel", value)?,
                    "target_env" => cfg_value(w, "abi", value)?,
                    _ => return Err(ErrorKind::CouldNotTranslateTarget.into())
                }
            },
            Cfg::Cfg(ref value) => cfg_value(w, "kernel", value)?,
        }
    }
    Ok(())
}

fn cfg_value(w: &mut Write, key: &str, value: &str) -> Result<(), Error> {
    match value {
        "macos" => write!(w, "{} == \"darwin\"", key)?,
        "unix" => write!(w, "({} == \"linux\" || {} == \"darwin\")", key, key)?,
        other => write!(w, "{} == \"{}\"", key, other)?,
    }
    Ok(())
}



named!(parens<Cfg>, ws!(delimited!( tag!("("), expr, tag!(")"))));
named!(parens_many<Vec<Cfg>>, ws!(delimited!(tag!("("), exprs, tag!(")"))));
named!(not<Cfg>, ws!(do_parse!(
    op: tag!("not") >> operands: parens >> (Cfg::Not(vec![operands]))
)));

named!(all<Cfg>, ws!(do_parse!(
    op: tag!("all") >> operands: parens_many >> (Cfg::Op { op: CfgOp::All, operands })
)));

named!(any<Cfg>, ws!(do_parse!(
    op: tag!("any") >> operands: parens_many >> (Cfg::Op { op: CfgOp::Any, operands })
)));

named!(expr<Cfg>, alt!(
    not | all | any | equal | simple
));

named!(simple<Cfg>, map!(ident, |a| Cfg::Cfg(from_utf8(a).unwrap().to_string())));
named!(equal<Cfg>, ws!(do_parse!(
    k: ident >>
        eq: tag!("=") >>
        v: delimited!(tag!("\""), take_till!(|c| c == b'"'), tag!("\"")) >>
        (Cfg::Equal(from_utf8(k).unwrap().to_string(),
                    from_utf8(v).unwrap().to_string()))
)));

named!(ident<&[u8]>, take_while1_s!(|c| (c >= b'a' && c <= b'z') || (c >= b'A' && c <= b'Z') || (c >= b'0' && c <= b'9') || c == b'_' || c == b'-'));

named!(exprs<Vec<Cfg>>, ws!(separated_list!(tag!(","), expr)));

named!(cfg<Vec<Cfg>>, ws!(alt!(
    do_parse!(op: tag!("cfg") >> operands: parens_many >> (operands)) | map!(simple, |x| vec![x])
)));

#[test]
fn test_cfg() {
    let s = "cfg( all (  unix, not( target_os =   \"emscripten\"  ), not( target_os = \"macos\"), not(target_os=\"ios\")))";
    // let s = "cfg(target_os=\"em\")";
    let x = cfg(s.as_bytes());
    if let IResult::Done(ref i, ref o) = x {
        println!("{:?}", o);
    } else {
        panic!("{:?}", x)
    }
}
