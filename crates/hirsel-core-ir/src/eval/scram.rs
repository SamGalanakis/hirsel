use super::*;

impl Machine<'_> {
    pub(super) fn scram_send(&mut self, t: &mut Task, args: &[u64]) -> X<Vec<u64>> {
        use hirsel_core_net::scram::Transcript;
        let key = args[0];
        let Some((name, spent, binding)) = self.world.tls_writes.get(&(key & !3)) else {
            return Ok(vec![0, 1, 0, 0]);
        };
        let secret = self.secret(args[1]).unwrap();
        if !secret
            .policy
            .send
            .iter()
            .any(|pattern| pattern.covers(name))
        {
            return Ok(vec![0, 1, 0, 0]);
        }
        let (spent, binding, password) = (*spent, *binding, secret.bytes.clone());
        let client_first = self.carrier_bytes(args[2]).to_vec();
        let server_first = self.carrier_bytes(args[3]).to_vec();
        let transcript = match Transcript::parse(&client_first, &server_first, &binding) {
            Ok(transcript) => transcript,
            Err(error) => return Ok(vec![0, error as u64, 0, 0]),
        };
        self.take_steps(t, transcript.iterations().into())?;
        let payer = t.limits.last().unwrap().clone();
        const CHARGE: u64 = 32;
        if let Err(available) = self.admit(&payer, CHARGE) {
            return Ok(vec![CHARGE, 6, 0, available]);
        }
        if spent {
            self.refund(payer, CHARGE);
            return Ok(vec![0, 5, 0, 5]);
        }
        if let Err(cancelled) = self.check_cancel(t) {
            self.refund(payer, CHARGE);
            return Err(cancelled);
        }
        let before = self.carrier_bytes(args[4]).to_vec();
        let after = self.carrier_bytes(args[5]).to_vec();
        let (message, signature) = transcript.derive(&password);
        if let Some((c, id, live)) = self.port_socket(key, 3) {
            // Only Send calls reach its server, until it took every byte
            // (native `served_proof`); a failure spends the half.
            let joined = Zeroizing::new([&before[..], &message, &after].concat());
            let mut at = 0;
            while at < joined.len() {
                let mut w = [0; 9];
                w[..4].copy_from_slice(&[5, id, (joined.len() - at) as u64, 0]);
                let fields = vec![vec![], joined[at..].to_vec(), vec![]];
                let deadline = self.until(args, 6);
                let row =
                    match self.port_call_with(t, c, w, fields, None, live, &mut None, deadline) {
                        Ok(row) => row,
                        Err(stop) => {
                            self.refund(payer, CHARGE);
                            self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
                            return Err(stop);
                        }
                    };
                if row[1] != 0 {
                    self.refund(payer, CHARGE);
                    self.world.tls_writes.get_mut(&(key & !3)).unwrap().1 = true;
                    return Ok(vec![0, 5, row[2], row[1]]);
                }
                at += (row[0] as usize).min(joined.len() - at);
            }
            let id = SCRAM_BASE + self.world.next_scram;
            self.world.next_scram += 1;
            self.world.scrams.insert(id, (signature, payer));
            return Ok(vec![id, 0, 0, 0]);
        }
        self.refund(payer, CHARGE);
        Ok(vec![0, 1, 0, 0])
    }

    pub(super) fn scram_final_len(&self, args: &[u64]) -> Vec<u64> {
        use hirsel_core_net::scram::Transcript;
        let Some((_, _, binding)) = self.world.tls_writes.get(&(args[0] & !3)) else {
            return vec![0, 1, 0, 0];
        };
        match Transcript::parse(
            self.carrier_bytes(args[1]),
            self.carrier_bytes(args[2]),
            binding,
        ) {
            Ok(transcript) => vec![transcript.final_len() as u64, 0, 0, 0],
            Err(error) => vec![0, error as u64, 0, 0],
        }
    }

    pub(super) fn scram_verify(&mut self, args: &[u64]) -> Vec<u64> {
        let (signature, payer) = self.world.scrams.remove(&args[0]).unwrap();
        let result = hirsel_core_net::scram::verify(&signature, self.carrier_bytes(args[1]));
        self.refund(payer, 32);
        vec![u64::from(result), 0, 0, 0]
    }
}
