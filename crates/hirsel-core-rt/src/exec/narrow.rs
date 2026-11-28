use crate::{
    Context,
    entry::{hirsel_core_capability_handle, hirsel_core_capability_provider},
    net::DENIED,
    provider::{Buffer, IoError, Outcome},
};
use hirsel_core_net::exec::{ArgToken, ExecRule, Form, Names};

pub(crate) struct NarrowedGrant {
    _grant: Option<Box<super::Grant>>,
    payer: crate::limit::Payer,
    charge: u64,
}

impl Drop for NarrowedGrant {
    fn drop(&mut self) {
        self.payer.refund(self.charge);
    }
}

// ArgToken's only dataful case is a Bytes carrier. The two empty cases
// use its first two reserved capacity words (MEM-13).
unsafe fn form(list: &Buffer) -> Option<Form> {
    let mut tokens = Vec::with_capacity(list.len);
    for i in 0..list.len {
        // SAFETY: the List[ArgToken] carrier pins every element. For a unit case
        // only its discriminant word is initialized.
        unsafe {
            let token = list.data.cast::<Buffer>().add(i);
            let tag = (&raw const (*token).capacity).read();
            tokens.push(match tag as u64 {
                CONST_ANY => ArgToken::Any,
                CONST_REST => ArgToken::Rest,
                _ => ArgToken::Lit(crate::tls::bytes(&*token).to_vec()),
            });
        }
    }
    Form::new(tokens)
}

const CONST_ANY: u64 = 1 << 63;
const CONST_REST: u64 = CONST_ANY + 1;

/// # Safety
/// Context belongs to this run. Exec is a live designation, and case, name
/// and argument come from a checked ExecRule whose carriers stay pinned here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_exec_narrow(
    context: *mut Context,
    exec: u64,
    case: u32,
    name: *const Buffer,
    argument: *const Buffer,
    site: u64,
) -> Outcome {
    unsafe {
        crate::process::deny_describe_capability(context);
        let name = std::str::from_utf8(crate::tls::bytes(&*name)).unwrap();
        let parse = || Names::parse(std::str::from_utf8(crate::tls::bytes(&*argument)).unwrap());
        let rule = match case {
            0 => Names::parse(name).map(ExecRule::Programs),
            1 => form(&*argument).map(|form| ExecRule::Form(name.to_owned(), form)),
            2 => parse().map(|keys| ExecRule::EnvKeys(name.to_owned(), keys)),
            3 => parse().map(|labels| ExecRule::Dirs(name.to_owned(), labels)),
            _ => unreachable!(),
        };
        let Some(rule) = rule else { return DENIED };
        let provider = *hirsel_core_capability_provider(context, exec);
        let key = hirsel_core_capability_handle(context, exec);
        if !crate::provide::grant_live(&provider, key) {
            return crate::provide::withdrawn(context, site);
        }
        #[cfg(feature = "record")]
        let taped = crate::tape::owns(&provider);
        #[cfg(feature = "record")]
        let (parent, rules) = if taped {
            let parent = crate::tape::source(&provider, key)
                .map(|(_, handle)| super::Grant::from_handle(handle));
            let Some(rules) = crate::tape::exec_rules(&provider, key) else {
                return DENIED;
            };
            (parent, rules.narrow(&rule))
        } else {
            let parent = super::Grant::from_handle(key);
            (Some(parent), parent.rules().narrow(&rule))
        };
        #[cfg(not(feature = "record"))]
        let (parent, rules) = {
            let parent = super::Grant::from_handle(key);
            (Some(parent), parent.rules().narrow(&rule))
        };
        let Some(rules) = rules else { return DENIED };
        let charge = rules.charge();
        let payer = (*context).payer();
        if let Err(capacity) = payer.admit_run_owned(charge) {
            return Outcome {
                value: capacity.requested,
                error: IoError { tag: 2, code: 0 },
                written: capacity.available,
                status: 0,
                kind: 0,
            };
        }
        let grant = parent.map(|parent| Box::new(parent.with_rules(rules.clone())));
        let handle = grant
            .as_deref()
            .map(|grant| std::ptr::from_ref(grant) as u64);
        #[cfg(feature = "record")]
        let handle = if taped {
            crate::tape::narrow_exec(&provider, key, handle, rules)
        } else {
            handle.unwrap()
        };
        #[cfg(not(feature = "record"))]
        let handle = handle.unwrap();
        let registry = &(*(*context).host_files).registry;
        registry
            .run_grants()
            .exec_grants
            .lock()
            .unwrap()
            .push(NarrowedGrant {
                _grant: grant,
                payer,
                charge: charge.unwrap(),
            });
        if !crate::provide::grant_child(&provider, key, handle) {
            return crate::provide::withdrawn(context, site);
        }
        Outcome {
            value: registry.grant(provider, handle),
            error: IoError::OK,
            written: 0,
            status: 0,
            kind: 0,
        }
    }
}
