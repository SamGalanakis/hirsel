// Review pending: langs-text-921. No matched ratio before independent review.
// Matches neural_net.hirsel: explicit LCG, indexed neuron/synapse vectors and checksum deep copy.
#[allow(dead_code)]
mod support;
const INPUT_00: [f64; 2] = [0.0, 0.0];
const INPUT_01: [f64; 2] = [0.0, 1.0];
const INPUT_10: [f64; 2] = [1.0, 0.0];
const INPUT_11: [f64; 2] = [1.0, 1.0];
const TARGET_0: [f64; 1] = [0.0];
const TARGET_1: [f64; 1] = [1.0];

#[derive(Clone)]
struct Synapse {
    weight: f64,
    prev_weight: f64,

    source_neuron: usize,
    dest_neuron: usize,
}

impl Synapse {
    fn new(source_neuron: usize, dest_neuron: usize, g: &mut support::Lcg) -> Self {
        let w = g.float(1.0) * 2.0 - 1.0;
        Self {
            weight: w,
            prev_weight: w,
            source_neuron,
            dest_neuron,
        }
    }
}

#[derive(Clone)]
struct Neuron {
    threshold: f64,
    prev_threshold: f64,
    output: f64,
    error: f64,
    synapses_in: Vec<usize>,
    synapses_out: Vec<usize>,
}

impl Neuron {
    const LEARNING_RATE: f64 = 1.0;
    const MOMENTUM: f64 = 0.3;

    fn new(g: &mut support::Lcg) -> Self {
        let t = g.float(1.0) * 2.0 - 1.0;
        Self {
            threshold: t,
            prev_threshold: t,
            output: 0.0,
            error: 0.0,
            synapses_in: Vec::new(),
            synapses_out: Vec::new(),
        }
    }

    fn derivative(&self) -> f64 {
        self.output * (1.0 - self.output)
    }

    fn calculate_output(&mut self, sources: &[Neuron], synapses: &[Synapse]) {
        let mut activation = 0.0;
        for &synapse_idx in &self.synapses_in {
            let synapse = &synapses[synapse_idx];
            activation += synapse.weight * sources[synapse.source_neuron].output;
        }
        activation -= self.threshold;
        self.output = 1.0 / (1.0 + (-activation).exp());
    }

    fn update_weights(&mut self, rate: f64, sources: &[Neuron], synapses: &mut [Synapse]) {
        for &synapse_idx in &self.synapses_in {
            let synapse = &mut synapses[synapse_idx];
            let source_output = sources[synapse.source_neuron].output;
            let temp_weight = synapse.weight;
            synapse.weight += (rate * Self::LEARNING_RATE * self.error * source_output)
                + (Self::MOMENTUM * (synapse.weight - synapse.prev_weight));
            synapse.prev_weight = temp_weight;
        }

        let temp_threshold = self.threshold;
        self.threshold += (rate * Self::LEARNING_RATE * self.error * -1.0)
            + (Self::MOMENTUM * (self.threshold - self.prev_threshold));
        self.prev_threshold = temp_threshold;
    }
}

#[derive(Clone)]
struct NeuralNetwork {
    input_layer: Vec<Neuron>,
    hidden_layer: Vec<Neuron>,
    output_layer: Vec<Neuron>,
    synapses: Vec<Synapse>,
}

impl NeuralNetwork {
    fn new(inputs: usize, hidden: usize, outputs: usize, g: &mut support::Lcg) -> Self {
        let mut input_layer: Vec<_> = (0..inputs).map(|_| Neuron::new(g)).collect();
        let mut hidden_layer: Vec<_> = (0..hidden).map(|_| Neuron::new(g)).collect();
        let mut output_layer: Vec<_> = (0..outputs).map(|_| Neuron::new(g)).collect();
        let mut synapses = Vec::new();

        Self::connect(&mut input_layer, &mut hidden_layer, &mut synapses, g);
        Self::connect(&mut hidden_layer, &mut output_layer, &mut synapses, g);

        Self {
            input_layer,
            hidden_layer,
            output_layer,
            synapses,
        }
    }

    fn connect(
        sources: &mut [Neuron],
        destinations: &mut [Neuron],
        synapses: &mut Vec<Synapse>,
        g: &mut support::Lcg,
    ) {
        for (source_idx, source) in sources.iter_mut().enumerate() {
            for (dest_idx, dest) in destinations.iter_mut().enumerate() {
                let synapse_idx = synapses.len();
                synapses.push(Synapse::new(source_idx, dest_idx, g));
                source.synapses_out.push(synapse_idx);
                dest.synapses_in.push(synapse_idx);
            }
        }
    }

    fn train(&mut self, inputs: &[f64], targets: &[f64]) {
        self.feed_forward(inputs);

        const RATE: f64 = 0.3;

        for (neuron, &target) in self.output_layer.iter_mut().zip(targets) {
            neuron.error = (target - neuron.output) * neuron.derivative();
            neuron.update_weights(RATE, &self.hidden_layer, &mut self.synapses);
        }

        for neuron in &mut self.hidden_layer {
            let mut sum = 0.0;
            for &synapse_idx in &neuron.synapses_out {
                let synapse = &self.synapses[synapse_idx];
                sum += synapse.prev_weight * self.output_layer[synapse.dest_neuron].error;
            }
            neuron.error = sum * neuron.derivative();
            neuron.update_weights(RATE, &self.input_layer, &mut self.synapses);
        }
    }

    fn feed_forward(&mut self, inputs: &[f64]) {
        for (neuron, &input) in self.input_layer.iter_mut().zip(inputs) {
            neuron.output = input;
        }

        for neuron in &mut self.hidden_layer {
            neuron.calculate_output(&self.input_layer, &self.synapses);
        }

        for neuron in &mut self.output_layer {
            neuron.calculate_output(&self.hidden_layer, &self.synapses);
        }
    }
}

fn run(c: &support::Config) -> u32 {
    assert_eq!(c.name, "Etc::NeuralNet");
    let mut g = support::Lcg::new();
    let mut net = NeuralNetwork::new(2, 10, 1, &mut g);
    for _ in 0..c.warmup + c.iterations {
        for _ in 0..1000 {
            net.train(&INPUT_00, &TARGET_0);
            net.train(&INPUT_10, &TARGET_1);
            net.train(&INPUT_01, &TARGET_1);
            net.train(&INPUT_11, &TARGET_0);
        }
    }
    let mut net_copy = net.clone();
    let mut all_outputs = Vec::new();
    for inputs in [&INPUT_00, &INPUT_01, &INPUT_10, &INPUT_11] {
        net_copy.feed_forward(inputs);
        all_outputs.push(net_copy.output_layer[0].output);
    }
    let mut sum = 0.0;
    for x in all_outputs {
        sum += x;
    }
    support::checksum(&format!("{:.7}", sum))
}
fn main() {
    support::main(run);
}
