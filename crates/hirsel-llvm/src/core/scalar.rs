//! Scalar lowering through the shared typed sink.
use super::*;

impl<S: sink::Sink> FunctionEmitter<'_, S> {
    pub(super) fn overflow(&mut self, t: Ty, op: ArithOp, a: &str, b: &str) -> (String, String) {
        let (name, signed) = match op { ArithOp::Add => ("add", t.int().is_some_and(|x| x.0)), ArithOp::Sub => ("sub", t.int().is_some_and(|x| x.0)), ArithOp::Mul => ("mul", t.int().is_some_and(|x| x.0)), ArithOp::Div => unreachable!() };
        if !signed && matches!(op, ArithOp::Add | ArithOp::Sub) {
            let value=self.typed_binary(if op==ArithOp::Add {sink::Binary::Add}else{sink::Binary::Sub},t,a,b);
            let flag=self.typed_compare(if op==ArithOp::Add {sink::Predicate::Ult}else{sink::Predicate::Ult},t,&value,a);
            return (value,flag);
        }
        let result=self.typed_call(&format!("llvm.{}{}.with.overflow.{}",if signed{"s"}else{"u"},name,ty(t)),t,&[(t,a),(t,b)]);
        let aggregate=sink::Type::structure(vec![ir_type(t),sink::Type::I1]);
        let rv=self.typed(sink::Operation::Extract(aggregate.clone(),&self.operand(&result),0));
        let rf=self.typed(sink::Operation::Extract(aggregate,&self.operand(&result),1));
        (rv,rf)
    }
    fn div_overflow(&mut self,t:Ty,a:&str,b:&str)->String {
        let zero=self.typed_compare(sink::Predicate::Eq,t,b,"0");
        if !t.int().is_some_and(|x| x.0) {return zero;}
        let min=(1u64<<(size(t)*8-1)).to_string();
        let x=self.typed_compare(sink::Predicate::Eq,t,a,&min);
        let y=self.typed_compare(sink::Predicate::Eq,t,b,"-1");
        let both=self.typed_binary(sink::Binary::And,Ty::Scalar(Scalar::Bool),&x,&y);
        self.typed_binary(sink::Binary::Or,Ty::Scalar(Scalar::Bool),&zero,&both)
    }
    pub(super) fn arith(&mut self,t:Ty,op:ArithOp,mode:ArithMode,a:&str,b:&str)->(String,String) {
        if op==ArithOp::Div {
            let over=self.div_overflow(t,a,b);
            let safe=self.typed_select(t,&over,"1",b);
            let x=self.typed_binary(if t.int().is_some_and(|x|x.0){sink::Binary::SDiv}else{sink::Binary::UDiv},t,a,&safe);
            let x=self.typed_select(t,&over,"0",&x);return (x,over);
        }
        match mode {
            ArithMode::Checked=>self.overflow(t,op,a,b),
            ArithMode::Wrapping=>(self.typed_binary(match op {ArithOp::Add=>sink::Binary::Add,ArithOp::Sub=>sink::Binary::Sub,ArithOp::Mul=>sink::Binary::Mul,_=>unreachable!()},t,a,b),"false".into()),
            ArithMode::Saturating=>{let (x,over)=self.overflow(t,op,a,b);let (signed,w)=t.int().unwrap();let limit=if !signed {if op==ArithOp::Sub{"0".into()}else{"-1".into()} } else {let neg=self.typed_compare(sink::Predicate::Slt,t,a,"0");self.typed_select(t,&neg,&(1u64<<(w-1)).to_string(),&((1u64<<(w-1))-1).to_string())};(self.typed_select(t,&over,&limit,&x),over)}
        }
    }
    pub(super) fn binary(&mut self,t:Ty,op:BinaryOp,a:&str,right:ValueId,fail:Option<BlockId>)->Result<String> {
        let b=self.numeric(right);if t.is_float() {
            let native=match op {BinaryOp::Add=>Some(sink::Binary::FAdd),BinaryOp::Sub=>Some(sink::Binary::FSub),BinaryOp::Mul=>Some(sink::Binary::FMul),BinaryOp::Div=>Some(sink::Binary::FDiv),_=>None};
            if let Some(op)=native{return Ok(self.typed_binary(op,t,a,&b));}
            let name=match op {BinaryOp::Min|BinaryOp::MinNumber=>"minimum",BinaryOp::Max|BinaryOp::MaxNumber=>"maximum",_=>unreachable!()};
            let ordered=self.typed_call(&format!("llvm.{name}.{}",if size(t)==4{"f32"}else{"f64"}),t,&[(t,a),(t,&b)]);return Ok(ordered);
        }
        let signed=t.int().is_some_and(|x|x.0);let width=(size(t)*8) as u32;
        Ok(match op {
            BinaryOp::Add|BinaryOp::Sub|BinaryOp::Mul=>{let ar=match op {BinaryOp::Add=>ArithOp::Add,BinaryOp::Sub=>ArithOp::Sub,_=>ArithOp::Mul};let (x,flag)=self.overflow(t,ar,a,&b);self.fail_if(&flag,fail,0)?;x}
            BinaryOp::Rem=>{let zero=self.typed_compare(sink::Predicate::Eq,t,&b,"0");self.fail_if(&zero,fail,2)?;let safe=if signed{let m=self.typed_compare(sink::Predicate::Eq,t,&b,"-1");self.typed_select(t,&m,"1",&b)}else{b.clone()};self.typed_binary(if signed{sink::Binary::SRem}else{sink::Binary::URem},t,a,&safe)}
            BinaryOp::Div=>{let zero=self.typed_compare(sink::Predicate::Eq,t,&b,"0");self.fail_if(&zero,fail,2)?;let over=self.div_overflow(t,a,&b);self.fail_if(&over,fail,0)?;self.typed_binary(if signed{sink::Binary::SDiv}else{sink::Binary::UDiv},t,a,&b)}
            BinaryOp::Shl|BinaryOp::Shr=>{if !failure::bounded_shift(t,self.constants[right.index()]){let outside=self.typed_compare(sink::Predicate::Uge,t,&b,&width.to_string());self.fail_if(&outside,fail,0)?;}self.typed_binary(if op==BinaryOp::Shl{sink::Binary::Shl}else if signed{sink::Binary::AShr}else{sink::Binary::LShr},t,a,&b)}
            BinaryOp::BitAnd|BinaryOp::BitOr|BinaryOp::BitXor=>self.typed_binary(match op {BinaryOp::BitAnd=>sink::Binary::And,BinaryOp::BitOr=>sink::Binary::Or,_=>sink::Binary::Xor},t,a,&b),
            BinaryOp::Min|BinaryOp::Max=>{let p=if signed {if op==BinaryOp::Min{sink::Predicate::Slt}else{sink::Predicate::Sgt}}else{if op==BinaryOp::Min{sink::Predicate::Ult}else{sink::Predicate::Ugt}};let c=self.typed_compare(p,t,a,&b);self.typed_select(t,&c,a,&b)}
            _=>unreachable!(),
        })
    }
    pub(super) fn scalar_unary(&mut self,t:Ty,op:ScalarUnaryOp,x:&str)->String {
        let name=match op {ScalarUnaryOp::Sqrt=>"sqrt",ScalarUnaryOp::Floor=>"floor",ScalarUnaryOp::Ceil=>"ceil",ScalarUnaryOp::Trunc=>"trunc",ScalarUnaryOp::Round=>"round",ScalarUnaryOp::RoundEven=>"roundeven",ScalarUnaryOp::Abs=>"fabs",ScalarUnaryOp::LeadingZeros=>"ctlz",ScalarUnaryOp::TrailingZeros=>"cttz",ScalarUnaryOp::CountOnes=>"ctpop",ScalarUnaryOp::SwapBytes=>"bswap",ScalarUnaryOp::ReverseBits=>"bitreverse"};
        if op==ScalarUnaryOp::SwapBytes&&size(t)==1{return x.into();}
        self.typed_call(&format!("llvm.{name}.{}",if t.is_float(){format!("f{}",size(t)*8)}else{ty(t)}),t,&[(t,x)])
    }
    pub(super) fn convert(&mut self,result:ValueId,flag:Option<ValueId>,operand:ValueId,mode:ConvertMode) {
        let from=self.vty(operand);let to=self.vty(result);let x=self.numeric(operand);let r=match mode {
            ConvertMode::Reinterpret if from==to=>x.clone(),ConvertMode::Reinterpret=>self.typed_cast(if from==Ty::Ptr{sink::Cast::PtrToInt}else if to==Ty::Ptr{sink::Cast::IntToPtr}else{sink::Cast::Bitcast},from,&x,to),
            ConvertMode::Widen if size(from)==size(to)=>x.clone(),ConvertMode::Widen=>self.typed_cast(if from.int().is_some_and(|v|v.0){sink::Cast::SExt}else{sink::Cast::ZExt},from,&x,to),
            ConvertMode::Truncate if size(from)==size(to)=>x.clone(),ConvertMode::Truncate=>self.typed_cast(sink::Cast::Trunc,from,&x,to),
            ConvertMode::IntToFloat=>self.typed_cast(if from.int().is_some_and(|v|v.0){sink::Cast::SIToFP}else{sink::Cast::UIToFP},from,&x,to),
            ConvertMode::FloatResize if size(from)<size(to)=>self.typed_cast(sink::Cast::FPExt,from,&x,to),ConvertMode::FloatResize=>self.typed_cast(sink::Cast::FPTrunc,from,&x,to),
            ConvertMode::TryNarrow=>{let (signed,w)=to.int().unwrap();let wide=self.typed_cast(if from.int().is_some_and(|v|v.0){sink::Cast::SExt}else{sink::Cast::ZExt},from,&x,Ty::Scalar(Scalar::I64));let lo=if signed{-(1i128<<(w-1))}else{0};let hi=if signed{(1i128<<(w-1))-1}else{(1i128<<w)-1};let a=self.typed_compare(sink::Predicate::Sge,Ty::Scalar(Scalar::I64),&wide,&lo.to_string());let b=self.typed_compare(sink::Predicate::Sle,Ty::Scalar(Scalar::I64),&wide,&hi.to_string());let ok=self.typed_binary(sink::Binary::And,Ty::Scalar(Scalar::Bool),&a,&b);if let Some(flag)=flag{let st=self.typed_cast(sink::Cast::ZExt,Ty::Scalar(Scalar::Bool),&ok,Ty::Scalar(Scalar::U8));self.put(flag,&st);}self.typed_cast(sink::Cast::Trunc,Ty::Scalar(Scalar::I64),&wide,to)}
            _=>return,
        };self.put(result,&r)
    }
}
