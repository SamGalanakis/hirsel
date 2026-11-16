//! FORM storage roles are disjoint within one activation. Scope declarations
//! let LLVM clone that activation's domains when it inlines the function.
use super::*;
use hirsel_core_ir::roles::{Facts, Role, Root};

pub(super) struct Scopes {
    addresses: Vec<Option<usize>>,
    scopes: Vec<usize>,
    whole_carriers: Vec<bool>,
}

impl Scopes {
    pub fn new(
        module: &Module,
        facts: &Facts,
        id: FuncId,
        name: &str,
        next: &mut usize,
        sink: &mut dyn Sink,
        items: &mut Items,
    ) -> Option<Self> {
        let function = &module.functions[id.index()];
        let addresses = facts.roles(module, id);
        let body = function.body.as_ref()?;
        let signature = &module.signatures[function.signature.index()];
        let params = body.blocks[0].params.get(&body.pools.values).unwrap();
        // Select whole mutable parameter roots only. A byte projection does not
        // authorize attaching the same scope to bytes beyond that projection.
        let whole_carriers = addresses
            .iter()
            .enumerate()
            .map(|(i, role)| {
                let a = &body.addresses[i];
                a.steps.len == 0
                    && match (role, a.base) {
                        (Some(Role::Carrier(Root::Param(i))), AddressBase::Value(value)) => {
                            matches!(
                                signature.params[*i as usize],
                                Param::Address {
                                    loan: Loan::Mut,
                                    ..
                                }
                            ) && params[usize::from(signature.dest.is_some()) + *i as usize]
                                == value
                        }
                        _ => false,
                    }
            })
            .collect();
        let mut unique = Vec::<Role>::new();
        let addresses = addresses
            .into_iter()
            .map(|role| {
                role.map(|role| {
                    if let Some(index) = unique.iter().position(|old| *old == role) {
                        index
                    } else {
                        unique.push(role);
                        unique.len() - 1
                    }
                })
            })
            .collect();
        if unique.len() < 2 {
            return None;
        }
        let domain = *next;
        *next += 1;
        sink.metadata(
            domain,
            &Metadata::Tuple {
                distinct: true,
                items: vec![MdItem::Node(domain), MdItem::String(name.into())],
            },
        );
        let mut scopes = Vec::new();
        for role in unique {
            let scope = *next;
            *next += 1;
            sink.metadata(
                scope,
                &Metadata::Tuple {
                    distinct: true,
                    items: vec![
                        MdItem::Node(scope),
                        MdItem::Node(domain),
                        MdItem::String(format!("{role:?}")),
                    ],
                },
            );
            scopes.push(scope);
        }
        items.declare(
            "llvm.experimental.noalias.scope.decl",
            FnSig::plain(Type::Void, [Type::Metadata]),
            FnAttrs::default(),
        );
        Some(Self {
            addresses,
            scopes,
            whole_carriers,
        })
    }

    pub fn declared(&self) -> &[usize] {
        &self.scopes
    }

    pub fn access(&self, addresses: &[AddressId]) -> Option<Scoped> {
        // A memcpy touches both operands. An unclassified side could touch any
        // role, so it prevents an assertion about the instruction as a whole.
        let Some(roles) = addresses
            .iter()
            .map(|a| self.addresses[a.index()])
            .collect::<Option<Vec<_>>>()
        else {
            return None;
        };
        let included: Vec<_> = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| roles.contains(i))
            .map(|(_, scope)| *scope)
            .collect();
        let excluded: Vec<_> = self
            .scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| !roles.contains(i))
            .map(|(_, scope)| *scope)
            .collect();
        Some(Scoped {
            alias: included,
            noalias: excluded,
        })
    }

    pub fn carrier(&self, address: AddressId) -> Option<Scoped> {
        if self.whole_carriers[address.index()] {
            self.access(&[address])
        } else {
            None
        }
    }
}

impl FunctionEmitter<'_> {
    /// Each scope's declaration lets the inliner clone this activation's domain.
    pub(super) fn declare_scopes(&mut self, scopes: &[usize]) {
        let sig = FnSig::plain(Type::Void, [Type::Metadata]);
        let callee = Value::global("llvm.experimental.noalias.scope.decl");
        for (i, scope) in scopes.iter().enumerate() {
            // The text producer attached the location to its last line only.
            let location = self.debug_location.filter(|_| i + 1 == scopes.len());
            let args = [Value::Metadata(vec![*scope])];
            self.sink.instruction(
                None,
                Operation::Call(&sig, &callee, &args, &CallAttrs::default()),
                location,
            );
        }
    }
}

// These types partition accesses, not pointers. The language cannot expose the
// Context step word to program storage. Runtime calls retain their ordinary
// memory effects; no claim is made about a runtime call's access to Context.
const STEP_ACCESS: usize = 5;
const PROGRAM_ACCESS: usize = 6;
pub(super) fn private_steps(sink: &mut dyn Sink) -> usize {
    for (id, items) in [
        (
            2,
            vec![MdItem::String("Hirsel private runtime fields".into())],
        ),
        (
            3,
            vec![
                MdItem::String("steps".into()),
                MdItem::Node(2),
                MdItem::I64(0),
            ],
        ),
        (
            4,
            vec![
                MdItem::String("program storage".into()),
                MdItem::Node(2),
                MdItem::I64(0),
            ],
        ),
        (
            STEP_ACCESS,
            vec![MdItem::Node(3), MdItem::Node(3), MdItem::I64(0)],
        ),
        (
            PROGRAM_ACCESS,
            vec![MdItem::Node(4), MdItem::Node(4), MdItem::I64(0)],
        ),
    ] {
        sink.metadata(
            id,
            &Metadata::Tuple {
                distinct: false,
                items,
            },
        );
    }
    7
}
pub(super) fn program_memory(memory: &Memory, facts: bool) -> Memory {
    let mut memory = memory.clone();
    if facts && memory.tbaa.is_none() {
        memory.tbaa = Some(PROGRAM_ACCESS);
    }
    memory
}
impl FunctionEmitter<'_> {
    pub(super) fn load_steps(&mut self, pointer: &Value) -> Value {
        self.load_with(
            &Type::I64,
            pointer,
            &Memory {
                tbaa: self.facts.then_some(STEP_ACCESS),
                ..Memory::aligned(8)
            },
        )
    }
    pub(super) fn store_steps(&mut self, value: &Value, pointer: &Value) {
        self.store_with(
            &Type::I64,
            value,
            pointer,
            &Memory {
                tbaa: self.facts.then_some(STEP_ACCESS),
                ..Memory::aligned(8)
            },
        );
    }
}
