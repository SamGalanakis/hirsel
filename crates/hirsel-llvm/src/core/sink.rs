//! LLVM syntax types shared by the streaming builder and the temporary text oracle.
//! These describe one operation at a time; no second instruction graph is retained.
use std::fmt::{self, Write};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Type {
    Void,
    Int(u32),
    Float,
    Double,
    Ptr,
    Array(u64, Box<Type>),
    Vector(u32, Box<Type>),
    Struct(Vec<Type>, bool),
}
impl Type {
    pub const I1: Self = Self::Int(1);
    pub const I8: Self = Self::Int(8);
    pub const I32: Self = Self::Int(32);
    pub const I64: Self = Self::Int(64);
    pub fn bytes(count: u64) -> Self { Self::Array(count, Box::new(Self::I8)) }
    pub fn structure(fields: impl Into<Vec<Self>>) -> Self { Self::Struct(fields.into(), false) }
}
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => f.write_str("void"),
            Self::Int(bits) => write!(f,"i{bits}"),
            Self::Float => f.write_str("float"),
            Self::Double => f.write_str("double"),
            Self::Ptr => f.write_str("ptr"),
            Self::Array(count, element) => write!(f,"[{count} x {element}]"),
            Self::Vector(count, element) => write!(f,"<{count} x {element}>"),
            Self::Struct(fields, packed) => {
                if *packed { f.write_char('<')?; }
                f.write_str("{ ")?;
                for (i,field) in fields.iter().enumerate() {
                    if i != 0 { f.write_str(", ")?; }
                    field.fmt(f)?;
                }
                f.write_str(" }")?;
                if *packed { f.write_char('>')?; }
                Ok(())
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Local {
    Temp(usize), Value(usize), Address(usize), Slot(usize), Param(usize),
    Context, Destination, CallerSite, Result, Sret, Payload, Input, Worklist,
}
impl fmt::Display for Local {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Temp(i) => write!(f,"%t{i}"),
            Self::Value(i) => write!(f,"%v{i}"),
            Self::Address(i) => write!(f,"%a{i}"),
            Self::Slot(i) => write!(f,"%s{i}"),
            Self::Param(i) => write!(f,"%p{i}"),
            Self::Context => f.write_str("%context"),
            Self::Destination => f.write_str("%dest"),
            Self::CallerSite => f.write_str("%caller_site"),
            Self::Result => f.write_str("%result"),
            Self::Sret => f.write_str("%sret"),
            Self::Payload => f.write_str("%payload"),
            Self::Input => f.write_str("%input"),
            Self::Worklist => f.write_str("%worklist"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Value {
    Local(Local),
    Integer(u128),
    Boolean(bool),
    Null,
    Poison,
    Zero,
    Global(String),
    Bitcast { from: Type, bits: u128, to: Type },
    IntToPtr(u64),
    PtrToInt(String),
    Add(Box<Value>, u64),
}
impl From<Local> for Value { fn from(local:Local)->Self { Self::Local(local) } }
impl From<u64> for Value { fn from(n:u64)->Self { Self::Integer(u128::from(n)) } }
impl From<bool> for Value { fn from(b:bool)->Self { Self::Boolean(b) } }
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(name) => name.fmt(f),
            Self::Integer(bits) => bits.fmt(f),
            Self::Boolean(value) => value.fmt(f),
            Self::Null => f.write_str("null"),
            Self::Poison => f.write_str("poison"),
            Self::Zero => f.write_str("zeroinitializer"),
            Self::Global(name) => write!(f,"@{name}"),
            Self::Bitcast { from,bits,to } => write!(f,"bitcast ({from} {bits} to {to})"),
            Self::IntToPtr(bits) => write!(f,"inttoptr (i64 {bits} to ptr)"),
            Self::PtrToInt(name) => write!(f,"ptrtoint (ptr @{name} to i64)"),
            Self::Add(left,right) => write!(f,"add (i64 {left}, i64 {right})"),
        }
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(super) enum Binary {
    Add, Sub, Mul, UDiv, SDiv, URem, SRem, And, Or, Xor, Shl, LShr, AShr,
    FAdd, FSub, FMul, FDiv, FRem,
}
impl Binary {
    pub fn name(self)-> &'static str {
        match self {
            Self::Add=>"add", Self::Sub=>"sub", Self::Mul=>"mul", Self::UDiv=>"udiv",
            Self::SDiv=>"sdiv", Self::URem=>"urem", Self::SRem=>"srem", Self::And=>"and",
            Self::Or=>"or", Self::Xor=>"xor", Self::Shl=>"shl", Self::LShr=>"lshr",
            Self::AShr=>"ashr", Self::FAdd=>"fadd", Self::FSub=>"fsub", Self::FMul=>"fmul",
            Self::FDiv=>"fdiv", Self::FRem=>"frem",
        }
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(super) enum Cast { Trunc,ZExt,SExt,FPToUI,FPToSI,UIToFP,SIToFP,FPTrunc,FPExt,PtrToInt,IntToPtr,Bitcast }
impl Cast {
    pub fn name(self)-> &'static str {
        match self {
            Self::Trunc=>"trunc",Self::ZExt=>"zext",Self::SExt=>"sext",Self::FPToUI=>"fptoui",
            Self::FPToSI=>"fptosi",Self::UIToFP=>"uitofp",Self::SIToFP=>"sitofp",Self::FPTrunc=>"fptrunc",
            Self::FPExt=>"fpext",Self::PtrToInt=>"ptrtoint",Self::IntToPtr=>"inttoptr",Self::Bitcast=>"bitcast",
        }
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub(super) enum Predicate {
    False=0, OEq=1, OGt=2, OGe=3, OLt=4, OLe=5, ONe=6, Ord=7,
    Uno=8, UEq=9, UGt=10, UGe=11, ULt=12, ULe=13, UNe=14, True=15,
    Eq=32, Ne=33, Ugt=34, Uge=35, Ult=36, Ule=37, Sgt=38, Sge=39, Slt=40, Sle=41,
}
impl Predicate {
    pub fn name(self)-> &'static str {
        match self {
            Self::False=>"false",Self::OEq=>"oeq",Self::OGt=>"ogt",Self::OGe=>"oge",Self::OLt=>"olt",Self::OLe=>"ole",Self::ONe=>"one",Self::Ord=>"ord",
            Self::Uno=>"uno",Self::UEq=>"ueq",Self::UGt=>"ugt",Self::UGe=>"uge",Self::ULt=>"ult",Self::ULe=>"ule",Self::UNe=>"une",Self::True=>"true",
            Self::Eq=>"eq",Self::Ne=>"ne",Self::Ugt=>"ugt",Self::Uge=>"uge",Self::Ult=>"ult",Self::Ule=>"ule",Self::Sgt=>"sgt",Self::Sge=>"sge",Self::Slt=>"slt",Self::Sle=>"sle",
        }
    }
    pub fn floating(self)->bool { (self as u32)<32 }
}
#[derive(Clone, Debug, Default)]
pub(super) struct Memory {
    pub align:u32,
    pub invariant:bool,
    pub alias_scopes:Vec<usize>,
    pub noalias_scopes:Vec<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Extension { None, Zero, Sign }
#[derive(Clone, Debug)]
pub(super) struct Parameter {
    pub ty:Type,
    pub extension:Extension,
    pub noalias:bool,
    pub readonly:bool,
    pub sret:Option<Type>,
    pub byval:Option<Type>,
}
impl Parameter {
    pub fn plain(ty:Type)->Self { Self { ty,extension:Extension::None,noalias:false,readonly:false,sret:None,byval:None } }
}
#[derive(Clone, Debug)]
pub(super) struct Signature {
    pub result:Type,
    pub extension:Extension,
    pub params:Vec<Parameter>,
}
#[derive(Clone, Debug, Default)]
pub(super) struct CallAttrs {
    pub cold:bool,
    pub tail:bool,
    pub alias_scopes:Vec<usize>,
    pub noalias_scopes:Vec<usize>,
}
/// One synchronous sink operation. The sink consumes it immediately.
pub(super) enum Operation<'a> {
    Binary(Binary,Type,&'a Value,&'a Value),
    Cast(Cast,Type,&'a Value,Type),
    Compare(Predicate,Type,&'a Value,&'a Value),
    Select(Type,&'a Value,&'a Value,&'a Value),
    FNeg(Type,&'a Value),
    Load(Type,&'a Value,&'a Memory),
    Store(Type,&'a Value,&'a Value,&'a Memory),
    Gep(Type,&'a Value,&'a [(Type,Value)]),
    Extract(Type,&'a Value,u32),
    Insert(Type,&'a Value,Type,&'a Value,u32),
    Phi(Type,&'a [(Value,String)]),
    Call(&'a Signature,&'a Value,&'a [Value],&'a CallAttrs),
    Branch(&'a str),
    Conditional(&'a Value,&'a str,&'a str,bool),
    Switch(Type,&'a Value,&'a str,&'a [(u64,String)]),
    Return(Type,Option<&'a Value>),
    Unreachable,
}
pub(super) trait Sink {
    // Temporary during the lowering port; removed before product cutover.
    fn raw(&mut self,_text:&str,_location:Option<usize>) { unimplemented!("typed lowering port in progress") }
    fn raw_alloca(&mut self,_text:&str) { unimplemented!("typed lowering port in progress") }

    fn instruction(&mut self, result:Option<Local>, op:Operation<'_>, location:Option<usize>);
    fn block(&mut self,name:&str);
    fn alloca(&mut self,name:Local,ty:&Type,align:u32);
}

/// Temporary parity oracle. Product lowering uses the native implementation.
#[derive(Default)]
pub(super) struct TextSink {
    pub allocations:String,
    pub code:String,
}
fn metadata(out:&mut String, memory:&Memory) {
    if memory.invariant { out.push_str(", !invariant.load !1"); }
    scope_list(out,"alias.scope",&memory.alias_scopes);
    scope_list(out,"noalias",&memory.noalias_scopes);
}
fn scope_list(out:&mut String, name:&str, ids:&[usize]) {
    if ids.is_empty() { return; }
    write!(out,", !{name} !{{").unwrap();
    for (i,id) in ids.iter().enumerate() {
        if i!=0 { out.push_str(", "); }
        write!(out,"!{id}").unwrap();
    }
    out.push('}');
}
fn parameter(out:&mut String,p:&Parameter) {
    write!(out,"{}",p.ty).unwrap();
    match p.extension {
        Extension::None=>{},Extension::Zero=>out.push_str(" zeroext"),Extension::Sign=>out.push_str(" signext"),
    }
    if p.noalias { out.push_str(" noalias"); }
    if p.readonly { out.push_str(" readonly"); }
    if let Some(ty)=&p.sret { write!(out," sret({ty})").unwrap(); }
    if let Some(ty)=&p.byval { write!(out," byval({ty})").unwrap(); }
}
impl Sink for TextSink {
    fn raw_alloca(&mut self,text:&str) {self.allocations.push_str(text);}
    fn raw(&mut self,text:&str,location:Option<usize>) {
        let location=location.filter(|_| !text.ends_with(':') && !text.ends_with('[')
            && !text.as_bytes().first().is_some_and(|c| *c==b'i' && text.as_bytes().get(1).is_some_and(u8::is_ascii_digit)));
        if let Some(id)=location {writeln!(self.code,"  {text}, !dbg !{id}").unwrap();}
        else {writeln!(self.code,"  {text}").unwrap();}
    }

    fn alloca(&mut self,name:Local,ty:&Type,align:u32) {
        write!(self.allocations,"  {name} = alloca {ty}").unwrap();
        if align!=0 { write!(self.allocations,", align {align}").unwrap(); }
        self.allocations.push('\n');
    }
    fn block(&mut self,name:&str) { writeln!(self.code,"{name}:").unwrap(); }
    fn instruction(&mut self,result:Option<Local>,op:Operation<'_>,location:Option<usize>) {
        let out=&mut self.code;
        out.push_str("  ");
        if let Some(name)=result { write!(out,"{name} = ").unwrap(); }
        match op {
            Operation::Binary(op,ty,a,b)=>write!(out,"{} {ty} {a}, {b}",op.name()).unwrap(),
            Operation::Cast(op,from,value,to)=>write!(out,"{} {from} {value} to {to}",op.name()).unwrap(),
            Operation::Compare(op,ty,a,b)=>write!(out,"{} {} {ty} {a}, {b}",if op.floating(){"fcmp"}else{"icmp"},op.name()).unwrap(),
            Operation::Select(ty,condition,yes,no)=>write!(out,"select i1 {condition}, {ty} {yes}, {ty} {no}").unwrap(),
            Operation::FNeg(ty,value)=>write!(out,"fneg {ty} {value}").unwrap(),
            Operation::Load(ty,p,memory)=>{
                write!(out,"load {ty}, ptr {p}").unwrap();
                if memory.align!=0 { write!(out,", align {}",memory.align).unwrap(); }
                metadata(out,memory);
            }
            Operation::Store(ty,value,p,memory)=>{
                write!(out,"store {ty} {value}, ptr {p}").unwrap();
                if memory.align!=0 { write!(out,", align {}",memory.align).unwrap(); }
                metadata(out,memory);
            }
            Operation::Gep(ty,p,indices)=>{
                write!(out,"getelementptr {ty}, ptr {p}").unwrap();
                for (ty,v) in indices { write!(out,", {ty} {v}").unwrap(); }
            }
            Operation::Extract(ty,value,index)=>write!(out,"extractvalue {ty} {value}, {index}").unwrap(),
            Operation::Insert(ty,aggregate,field,value,index)=>write!(out,"insertvalue {ty} {aggregate}, {field} {value}, {index}").unwrap(),
            Operation::Phi(ty,incoming)=>{
                write!(out,"phi {ty} ").unwrap();
                for (i,(value,block)) in incoming.iter().enumerate() {
                    if i!=0 { out.push_str(", "); }
                    write!(out,"[{value}, %{block}]").unwrap();
                }
            }
            Operation::Call(sig,callee,args,attrs)=>{
                if attrs.tail { out.push_str("tail "); }
                out.push_str("call ");
                match sig.extension { Extension::None=>{},Extension::Zero=>out.push_str("zeroext "),Extension::Sign=>out.push_str("signext ") }
                write!(out,"{} {callee}(",sig.result).unwrap();
                for (i,(param,arg)) in sig.params.iter().zip(args).enumerate() {
                    if i!=0 { out.push_str(", "); }
                    parameter(out,param);
                    write!(out," {arg}").unwrap();
                }
                out.push(')');
                if attrs.cold { out.push_str(" cold"); }
                scope_list(out,"alias.scope",&attrs.alias_scopes);
                scope_list(out,"noalias",&attrs.noalias_scopes);
            }
            Operation::Branch(target)=>write!(out,"br label %{target}").unwrap(),
            Operation::Conditional(test,yes,no,likely)=>{
                write!(out,"br i1 {test}, label %{yes}, label %{no}").unwrap();
                if likely { out.push_str(", !prof !0"); }
            }
            Operation::Switch(ty,value,default,arms)=>{
                write!(out,"switch {ty} {value}, label %{default} [").unwrap();
                for (value,label) in arms { write!(out,"\n  {ty} {value}, label %{label}").unwrap(); }
                out.push_str("\n  ]");
            }
            Operation::Return(ty,Some(value))=>write!(out,"ret {ty} {value}").unwrap(),
            Operation::Return(_,None)=>out.push_str("ret void"),
            Operation::Unreachable=>out.push_str("unreachable"),
        }
        if let Some(location)=location { write!(out,", !dbg !{location}").unwrap(); }
        out.push('\n');
    }
}

#[cfg(hirsel_lazy_core)]
mod native;

#[cfg(all(test,hirsel_lazy_core))]
pub(crate) fn primitive_oracle() {
    fn build(sink:&mut impl Sink) {
        sink.block("entry");
        sink.alloca(Local::Slot(0),&Type::I64,8);
        sink.instruction(Some(Local::Temp(0)),Operation::Binary(Binary::Add,Type::I64,&Local::Param(0).into(),&1u64.into()),None);
        sink.instruction(Some(Local::Value(0)),Operation::Select(Type::I64,&true.into(),&Local::Temp(0).into(),&Value::Poison),None);
        sink.instruction(None,Operation::Branch("producer"),None);
        sink.block("done");
        sink.instruction(None,Operation::Store(Type::I64,&Local::Temp(1).into(),&Local::Slot(0).into(),&Memory::default()),None);
        sink.instruction(Some(Local::Temp(2)),Operation::Load(Type::I64,&Local::Slot(0).into(),&Memory::default()),None);
        sink.instruction(None,Operation::Return(Type::I64,Some(&Local::Temp(2).into())),None);
        sink.block("producer");
        sink.instruction(Some(Local::Temp(1)),Operation::Binary(Binary::Add,Type::I64,&Local::Value(0).into(),&1u64.into()),None);
        sink.instruction(None,Operation::Branch("done"),None);
    }
    let context=crate::construction::Context::new().unwrap();
    let mut native=native::NativeSink::new(context.unit());
    let sig=Signature {result:Type::I64,extension:Extension::None,params:vec![Parameter::plain(Type::I64)]};
    native.begin_function("fixture",&sig,&[Local::Param(0)],false);
    build(&mut native);
    let mut text=TextSink::default();build(&mut text);
    let text=format!("define i64 @fixture(i64 %p0) {{\n{} }}",text.code.replacen("entry:\n",&format!("entry:\n{}",text.allocations),1));
    assert_eq!(native.unit.print().unwrap(),context.print_text(&text).unwrap());
}
