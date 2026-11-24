use super::*;

pub(super) fn install(mach: &mut Machine<'_>, f: FuncId, revisions: Option<&Revisions>) {
    if let Some(revisions) = revisions {
        let revision = match mach.m.revision_root(f) {
            RevisionRoot::Item(identity) => revisions.get(&identity.digest()).copied(),
            RevisionRoot::Intrinsic(identity) => Some(intrinsic_revision(&identity)),
        };
        let launch = mach.world.launch.as_mut().unwrap();
        if launch.revision().is_none() {
            *launch = launch
                .clone()
                .with_launch_identity(revision.map(|digest| digest.0), launch.n_place_explicit());
        }
    }
    let target = mach
        .world
        .launch
        .as_ref()
        .unwrap()
        .target()
        .as_bytes()
        .to_vec();
    if !target.is_empty() {
        let target_data = mach.block(target.len() as u64, None, 0, None);
        mach.mem[split(target_data).0].bytes.clone_from(&target);
        mach.mem[split(target_data).0].fixed = true;
        mach.world.launch_target = (target_data, target.len() as u64);
    }
}
