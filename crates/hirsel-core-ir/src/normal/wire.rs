use super::*;

pub(super) struct Write<'order, 'm, 'b> {
    pub(super) bytes: Vec<u8>,
    pub(super) module: &'order mut ModuleOrder<'m>,
    pub(super) local: Option<BodyOrder<'b>>,
    pub(super) closure: bool,
    pub(super) abi: bool,
}

impl<'order, 'm, 'b> Write<'order, 'm, 'b> {
    pub(super) fn module(module: &'order mut ModuleOrder<'m>) -> Self {
        Self {
            bytes: Vec::new(),
            module,
            local: None,
            closure: false,
            abi: false,
        }
    }

    pub(super) fn local(&self) -> &BodyOrder<'b> {
        self.local
            .as_ref()
            .expect("a local reference is inside a body")
    }

    pub(super) fn local_mut(&mut self) -> &mut BodyOrder<'b> {
        self.local
            .as_mut()
            .expect("a local reference is inside a body")
    }

    pub(super) fn raw(&mut self, bytes: &[u8]) {
        if !self.module.planning {
            self.bytes.extend_from_slice(bytes);
        }
    }

    pub(super) fn count(&mut self, value: usize) {
        if self.module.planning {
            return;
        }
        if self.closure {
            (value as u64).write(self);
        } else {
            uleb(&mut self.bytes, value as u64);
        }
    }

    fn local_row(&mut self, kind: LocalRow, row: usize) {
        let (number, fresh) = self.local_mut().table(kind).discover(row);
        self.count(number as usize);
        if fresh {
            let body = self.local().body;
            let prior = std::mem::take(&mut self.bytes);
            match kind {
                LocalRow::Slot => body.slots[row].write(self),
                LocalRow::Address => body.addresses[row].write(self),
                LocalRow::Call => body.calls[row].write(self),
                LocalRow::Edge => body.edges[row].write(self),
            }
            let bytes = std::mem::replace(&mut self.bytes, prior);
            self.local_mut().table(kind).bytes[number as usize] = bytes;
        }
    }
}

pub(super) struct Read<'a> {
    bytes: &'a [u8],
    pub(super) pools: Pools,
    pub(super) instance_closures: Vec<Vec<ContentHash>>,
    global_refs: [Option<u32>; 5],
    local_refs: [Option<u32>; 6],
}

impl<'a> Read<'a> {
    pub(super) fn new(bytes: &'a [u8], magic: &[u8]) -> io::Result<Self> {
        let bytes = bytes
            .strip_prefix(magic)
            .ok_or_else(|| invalid("normal form header"))?;
        Ok(Self {
            bytes,
            pools: Pools::default(),
            instance_closures: Vec::new(),
            global_refs: [None; 5],
            local_refs: [None; 6],
        })
    }

    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let (head, tail) = self
            .bytes
            .split_at_checked(n)
            .ok_or_else(|| invalid("truncated normal form"))?;
        self.bytes = tail;
        Ok(head)
    }

    fn unsigned(&mut self) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let byte = u8::read(self)?;
            if shift == 63 && byte > 1 {
                break;
            }
            value |= u64::from(byte & 127) << shift;
            if byte < 128 {
                if shift == 0 || byte != 0 {
                    return Ok(value);
                }
                break;
            }
        }
        Err(invalid("invalid unsigned LEB128"))
    }

    pub(super) fn index(&mut self) -> io::Result<u32> {
        self.unsigned()?
            .try_into()
            .map_err(|_| invalid("index exceeds u32"))
    }

    pub(super) fn count(&mut self) -> io::Result<usize> {
        let count =
            usize::try_from(self.unsigned()?).map_err(|_| invalid("count exceeds usize"))?;
        if count > self.bytes.len() {
            return Err(invalid("count exceeds remaining bytes"));
        }
        Ok(count)
    }

    fn reference(&mut self, global: bool, kind: usize) -> io::Result<u32> {
        let id = self.index()?;
        let max = if global {
            &mut self.global_refs[kind]
        } else {
            &mut self.local_refs[kind]
        };
        *max = (*max).max(Some(id));
        Ok(id)
    }

    fn check_refs<const N: usize>(refs: [Option<u32>; N], limits: [usize; N]) -> io::Result<()> {
        if refs
            .into_iter()
            .zip(limits)
            .all(|(max, len)| max.is_none_or(|id| (id as usize) < len))
        {
            Ok(())
        } else {
            Err(invalid("reference outside normal form table"))
        }
    }

    pub(super) fn check_globals(&self, module: &Module) -> io::Result<()> {
        Self::check_refs(
            self.global_refs,
            [
                module.types.len(),
                module.signatures.len(),
                module.statics.len(),
                module.instances.len(),
                module.functions.len(),
            ],
        )
    }

    pub(super) fn check_locals(&mut self, body: &Body) -> io::Result<()> {
        Self::check_refs(
            std::mem::take(&mut self.local_refs),
            [
                body.blocks.len(),
                body.values.len(),
                body.slots.len(),
                body.addresses.len(),
                body.calls.len(),
                body.edges.len(),
            ],
        )
    }

    pub(super) fn end(self) -> io::Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(invalid("trailing normal form bytes"))
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) trait Wire: Sized {
    fn write(&self, out: &mut Write<'_, '_, '_>);
    fn read(input: &mut Read<'_>) -> io::Result<Self>;
}

macro_rules! integers {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                out.raw(&self.to_le_bytes());
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                Ok(Self::from_le_bytes(input.take(std::mem::size_of::<Self>())?.try_into().unwrap()))
            }
        }
    )*};
}
integers!(u8, u32, u64);

impl Wire for bool {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        u8::from(*self).write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        match u8::read(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("invalid boolean")),
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        self.is_some().write(out);
        if let Some(value) = self {
            value.write(out);
        }
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        bool::read(input)?.then(|| T::read(input)).transpose()
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        write_slice(self, out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        (0..input.count()?).map(|_| T::read(input)).collect()
    }
}

impl<T: Wire> Wire for Box<T> {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        (**self).write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Box::new(T::read(input)?))
    }
}

impl Wire for String {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        out.count(self.len());
        out.raw(self.as_bytes());
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        let n = input.count()?;
        String::from_utf8(input.take(n)?.to_vec()).map_err(|_| invalid("invalid UTF-8"))
    }
}

pub(super) fn write_slice<T: Wire>(values: &[T], out: &mut Write<'_, '_, '_>) {
    out.count(values.len());
    for value in values {
        value.write(out);
    }
}

macro_rules! rows {
    ($($ty:ident : $global:literal, $kind:literal => $visit:expr),* $(,)?) => {$(
        impl Wire for $ty {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                if out.closure {
                    self.0.write(out);
                } else if out.abi {
                    out.module.module.closure(TypeId(self.0)).hash.write(out);
                } else {
                    let number = ($visit)(out, self.0 as usize);
                    out.count(number as usize);
                }
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                input.reference($global, $kind).map(Self)
            }
        }
    )*};
}
rows! {
    TypeId: true, 0 => |out: &mut Write<'_, '_, '_>, row| out.module.row(GlobalRow::Type, row),
    TypeRef: true, 0 => |out: &mut Write<'_, '_, '_>, row| out.module.row(GlobalRow::Type, row),
    SigId: true, 1 => |out: &mut Write<'_, '_, '_>, row| out.module.row(GlobalRow::Signature, row),
    StaticId: true, 2 => |out: &mut Write<'_, '_, '_>, row| out.module.row(GlobalRow::Static, row),
    InstanceId: true, 3 => |out: &mut Write<'_, '_, '_>, row| out.module.row(GlobalRow::Instance, row),
    FuncId: true, 4 => |out: &mut Write<'_, '_, '_>, row| out.module.function(row),
    FuncRef: true, 4 => |out: &mut Write<'_, '_, '_>, row| out.module.function(row),
    BlockId: false, 0 => |out: &mut Write<'_, '_, '_>, row| out.local().blocks.get(row),
    ValueId: false, 1 => |out: &mut Write<'_, '_, '_>, row| out.local().values.get(row),
}

macro_rules! local_rows {
    ($($ty:ident => $row:ident, $kind:literal),*) => {$(
        impl Wire for $ty {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                out.local_row(LocalRow::$row, self.index());
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                input.reference(false, $kind).map(Self)
            }
        }
    )*};
}
local_rows!(SlotId => Slot, 2, AddressId => Address, 3, CallId => Call, 4, EdgeId => Edge, 5);

macro_rules! pools {
    ($($ty:ty => $pool:ident),*) => {$(
        impl Wire for PoolRange<$ty> {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                let pool = &out.local().body.pools.$pool;
                write_slice(self.get(pool).expect("checked pool range"), out);
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                let items = Vec::<$ty>::read(input)?;
                Ok(Self::push(&mut input.pools.$pool, items))
            }
        }
    )*};
}
pools!(ValueId => values, Step => steps, Arg => args, SwitchArm => arms);

macro_rules! record {
    ($ty:ty { $($field:ident),* $(,)? }) => {
        impl Wire for $ty {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                let Self { $($field),* } = self;
                $($field.write(out);)*
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                Ok(Self { $($field: Wire::read(input)?),* })
            }
        }
    };
}

macro_rules! wire_field {
    (write $out:ident, $field:ident) => {
        $field.write($out)
    };
    (write $out:ident, $field:ident @ index) => {
        $out.count(*$field as usize)
    };
    (write $out:ident, $field:ident @ optional_index) => {{
        $field.is_some().write($out);
        if let Some(index) = $field {
            $out.count(*index as usize);
        }
    }};
    (read $input:ident, $field:ident) => {
        Wire::read($input)
    };
    (read $input:ident, $field:ident @ index) => {
        $input.index()
    };
    (read $input:ident, $field:ident @ optional_index) => {{ bool::read($input)?.then(|| $input.index()).transpose() }};
}

macro_rules! tags {
    ($ty:ty { $($tag:literal => $name:ident $( ( $($part:ident $(@ $part_kind:ident)?),+ ) )? $( { $($field:ident $(@ $field_kind:ident)?),+ } )?),* $(,)? }) => {
        impl Wire for $ty {
            fn write(&self, out: &mut Write<'_, '_, '_>) {
                match self {
                    $(Self::$name $(( $($part),+ ))? $({ $($field),+ })? => {
                        if out.closure {
                            ($tag as u32).write(out);
                        } else {
                            ($tag as u8).write(out);
                        }
                        $($(wire_field!(write out, $part $(@ $part_kind)?);)+)?
                        $($(wire_field!(write out, $field $(@ $field_kind)?);)+)?
                    }),*
                }
            }

            fn read(input: &mut Read<'_>) -> io::Result<Self> {
                match u8::read(input)? {
                    $($tag => Ok(Self::$name $(( $(wire_field!(read input, $part $(@ $part_kind)?)?),+ ))? $({ $($field: wire_field!(read input, $field $(@ $field_kind)?)?),+ })?)),*,
                    _ => Err(invalid(concat!("unknown ", stringify!($ty), " tag"))),
                }
            }
        }
    };
}

impl Wire for DefPath {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        if out.closure {
            self.0.write(out);
        } else {
            self.definition_identity().write(out);
        }
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        let text = String::read(input)?;
        let mut parts = text.split('/');
        let (package, version) = parts
            .next()
            .and_then(|p| p.split_once('@'))
            .ok_or_else(|| invalid("missing package version"))?;
        fn component(text: &str) -> io::Result<String> {
            let mut input = text.as_bytes().iter().copied();
            let mut bytes = Vec::with_capacity(text.len());
            while let Some(byte) = input.next() {
                match byte {
                    b'%' => {
                        let high = input.next().and_then(|b| (b as char).to_digit(16));
                        let low = input.next().and_then(|b| (b as char).to_digit(16));
                        let value = high.zip(low).ok_or_else(|| invalid("bad path escape"))?;
                        bytes.push((value.0 * 16 + value.1) as u8);
                    }
                    b => bytes.push(b),
                }
            }
            String::from_utf8(bytes).map_err(|_| invalid("invalid path UTF-8"))
        }
        let mut modules: Vec<_> = parts.map(component).collect::<io::Result<_>>()?;
        let name = modules.pop().ok_or_else(|| invalid("missing item name"))?;
        let path = Self(ItemPath {
            package: PackageId {
                name: component(package)?,
                version: version.to_owned(),
            },
            modules,
            name,
        });
        if path.definition_identity() != text
            || !version.bytes().all(|b| b.is_ascii_digit())
            || version.is_empty()
            || (version.len() > 1 && version.starts_with('0'))
        {
            return Err(invalid("noncanonical definition identity"));
        }
        Ok(path)
    }
}

impl Wire for ContentHash {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        out.raw(&self.0);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Self(input.take(32)?.try_into().unwrap()))
    }
}

record!(Identity {
    definition,
    dictionaries,
    closures
});

impl Wire for InstanceKey {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        self.definition.write(out);
        self.dictionaries.write(out);
        out.count(self.arguments.len());
        if !out.module.planning {
            for &arg in &self.arguments {
                out.module.module.closure(arg).hash.write(out);
            }
        }
        let prior = std::mem::replace(&mut out.module.metadata, true);
        self.arguments.write(out);
        out.module.metadata = prior;
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        let Identity {
            definition,
            dictionaries,
            closures,
        } = Wire::read(input)?;
        let arguments: Vec<TypeId> = Wire::read(input)?;
        if arguments.len() != closures.len() {
            return Err(invalid("instance argument and closure counts differ"));
        }
        input.instance_closures.push(closures);
        Ok(Self {
            definition,
            dictionaries,
            arguments,
        })
    }
}

pub(super) fn encode_closure(rows: &[ClosureRow], out: &mut Write<'_, '_, '_>) {
    out.closure = true;
    out.count(rows.len());
    for row in rows {
        row.shape.write(out);
        for hook in &row.hooks {
            hook.write(out);
        }
    }
}

record!(ClosureHook {
    definition,
    dictionaries,
    arguments,
    signature
});
record!(PackageId { name, version });
record!(ItemPath {
    package,
    modules,
    name
});
record!(Signature {
    params,
    results,
    dest,
    never
});
record!(Field { name, ty });
record!(Case { name, fields });
record!(AddressInfo { base, steps, ty });

impl Wire for CallSite {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        let Self {
            callee,
            signature,
            args,
            results,
            fail,
            cold: _,
        } = self;
        callee.write(out);
        signature.write(out);
        args.write(out);
        results.write(out);
        fail.write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Self {
            callee: Wire::read(input)?,
            signature: Wire::read(input)?,
            args: Wire::read(input)?,
            results: Wire::read(input)?,
            fail: Wire::read(input)?,
            cold: false,
        })
    }
}

impl Wire for Edge {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        let Self {
            target,
            args,
            cold: _,
        } = self;
        target.write(out);
        args.write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Self {
            target: Wire::read(input)?,
            args: Wire::read(input)?,
            cold: false,
        })
    }
}

record!(SwitchArm { value, edge });

impl Wire for CompactRoot {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        self.ty.write(out);
        self.width.write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Self {
            ty: Wire::read(input)?,
            width: Wire::read(input)?,
        })
    }
}

impl Wire for Shape {
    fn write(&self, out: &mut Write<'_, '_, '_>) {
        let Self {
            name,
            repr,
            carried,
            body,
            finalizer,
            clone,
            codec,
            grant,
            send,
            held,
        } = self;
        name.write(out);
        repr.write(out);
        carried.write(out);
        body.write(out);
        if out.closure {
            finalizer.write(out);
            clone.write(out);
            codec.write(out);
            grant.write(out);
            send.write(out);
            held.write(out);
            return;
        }
        held.write(out);
        let hooks = [finalizer, clone, codec];
        let prior = std::mem::replace(&mut out.module.metadata, true);
        let module = out.module.module;
        for hook in hooks.into_iter().filter_map(|f| *f) {
            // Close the hook's generic arguments before any hook signature.
            out.module.row(
                GlobalRow::Instance,
                module.functions[hook.0 as usize].instance.index(),
            );
        }
        for hook in hooks.into_iter().filter_map(|f| *f) {
            out.module.row(
                GlobalRow::Signature,
                module.functions[hook.0 as usize].signature.index(),
            );
        }
        out.module.metadata = prior;
        if out.module.planning {
            for hook in hooks.into_iter().filter_map(|f| *f) {
                out.module.declarations.number(hook.0 as usize);
            }
            return;
        }
        finalizer.write(out);
        clone.write(out);
        codec.write(out);
        grant.write(out);
        send.write(out);
    }

    fn read(input: &mut Read<'_>) -> io::Result<Self> {
        Ok(Self {
            name: Wire::read(input)?,
            repr: Wire::read(input)?,
            carried: Wire::read(input)?,
            body: Wire::read(input)?,
            held: Wire::read(input)?,
            finalizer: Wire::read(input)?,
            clone: Wire::read(input)?,
            codec: Wire::read(input)?,
            grant: Wire::read(input)?,
            send: Wire::read(input)?,
        })
    }
}

record!(Site { file, line, column });

mod tags {
    use super::*;
    include!("tags.rs");
}
