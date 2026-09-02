const fs=require('fs');
function edit(p,f){let s=fs.readFileSync(p,'utf8');const rep=(a,b,all)=>{if(!s.includes(a))throw new Error(p+' missing: '+a.slice(0,70));s=all?s.split(a).join(b):s.replace(a,b);};f(rep);fs.writeFileSync(p,s);}
edit('src/params.rs',(rep)=>{
rep(`    /// Whether the knob is a value of the graph or a grip on other knobs.
    /// The rigid gain is the only one of the latter: it reads as the mean of
    /// the three channel knobs and turns all three, so it is a reading rather
    /// than a field — which is what [\`Params::knob_mut\`]'s \`unreachable!\`
    /// says too. Anything walking the graph's *values* wants the fields, and
    /// would otherwise name a knob no config can write when a channel is at
    /// fault.`,`    /// Whether the knob is a value of the graph or a grip on other knobs.
    /// The rigid gain is the only one of the latter: it reads as the mean of
    /// the three channel knobs and turns all three, so it is a reading rather
    /// than a field. Anything walking the graph's *values* wants the fields,
    /// and would otherwise name a knob no config can write when a channel is
    /// at fault. Not the same fact as [\`Params::knob_mut\`]'s reach: the
    /// delay owns a field too, a count of frames that is no \`f32\`, so a walk
    /// over the fields goes through [\`Params::set\`] and never \`knob_mut\`.`);
rep(`    pub fn is_on(self, params: &Params) -> bool {
        match self {
            Knob::Send => params.count(Node::Input) > 0,
            Knob::Delay => params.delay > 0,
            _ => true,
        }
    }

    /// Why [\`Knob::is_on\`] said no, for the refusal.
    pub fn why_off(self, params: &Params) -> String {
        match self {
            Knob::Send => format!(
                "a level on an input, and this graph has {}",
                params.count(Node::Input)
            ),
            Knob::Delay => "a frame delay unit, and this graph's reach is 0".to_string(),
            _ => unreachable!("{:?} is on every graph", self),
        }
    }`,`    pub fn is_on(self, params: &Params) -> bool {
        self.off_because(params).is_none()
    }

    /// Why the graph has nothing for this knob to act on, for the refusal.
    pub fn off_because(self, params: &Params) -> Option<String> {
        match self {
            Knob::Send if params.count(Node::Input) == 0 => {
                Some("a level on an input, and this graph has none".to_string())
            }
            Knob::Delay if params.delay == 0 => {
                Some("a frame delay unit, and this graph's reach is 0".to_string())
            }
            _ => None,
        }
    }`);
rep(`    /// of the longest delay any camera asks for.`,`    /// of the graph's reach.`);
rep(`        // The delay is a count of frames: the nearest whole one, inside the
        // reach the graph bought.
        if knob == Knob::Delay {`,`        if knob == Knob::Delay {`);
rep(`        if knob == Knob::Gain {
            let step = rigid_gain_step(&self.cameras[focus.camera].gain, delta);`,`        if knob == Knob::Gain {
            let ends = knob.limit(self).ends();
            let step = rigid_gain_step(&self.cameras[focus.camera].gain, delta, ends);`);
rep(`fn rigid_gain_step(gain: &[f32; 3], delta: f32) -> f32 {
    let (low, high) = Knob::Gain.limit(&identity_graph()).ends();`,`fn rigid_gain_step(gain: &[f32; 3], delta: f32, (low, high): (f32, f32)) -> f32 {`);
rep(`impl Camera {
    /// The most reach a graph's delay units may have: the thirty frames the
    /// original's dial up to. A bound because the reach is bought in bank:
    /// every frame of it is another copy of every monitor.
    pub const MAX_DELAY: u32 = 30;
}`,`impl Params {
    /// The most reach a graph's delay units may have: the thirty frames the
    /// original's dial up to. A bound because the reach is bought in bank:
    /// every frame of it is another copy of every monitor.
    pub const MAX_DELAY: u32 = 30;
}`);
rep(`        assert!(params.describe(focus).contains("delay 4/4"));`,`        assert!(params.describe(focus).contains("delay 4/4"));
        params.set(Knob::Delay, 3.0, focus);
        assert!(params.describe(focus).contains("delay 3/4"), "cable, then reach");`);
rep(`            let (low, high) = knob.limit(&identity_graph()).ends();
            let at = knob.identity();`,`            let (low, high) = knob.limit(&p()).ends();
            let at = knob.identity();`);
});
edit('src/feedback.rs',(rep)=>{
rep(`/// and the layer count comes from the graph and its longest delay, so no one`,`/// and the layer count comes from the graph and its reach, so no one`);
rep(`            "the graph's monitors, inputs and longest delay are baked into the bank at creation"`,`            "the graph's monitors, inputs and reach are baked into the bank at creation"`);
});
edit('src/affine.rs',(rep)=>{
rep(`/// The two ways a picture is mirrored, in the order the pair of flips is
/// written down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    pub const ALL: [Axis; 2] = [Axis::X, Axis::Y];
}

impl Framing {
    pub fn mirror(&mut self, axis: Axis) -> &mut bool {
        match axis {
            Axis::X => &mut self.flip_x,
            Axis::Y => &mut self.flip_y,
        }
    }

    pub fn mirrored(&self) -> [bool; 2] {
        [self.flip_x, self.flip_y]
    }
}
`,`/// In the order the pair of flips is written down, which is the order
/// [\`Framing::flipped\`] reads them out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    pub const ALL: [Axis; 2] = [Axis::X, Axis::Y];
}

impl Framing {
    pub fn flip(&mut self, axis: Axis) {
        let flip = match axis {
            Axis::X => &mut self.flip_x,
            Axis::Y => &mut self.flip_y,
        };
        *flip = !*flip;
    }

    pub fn flipped(&self) -> [bool; 2] {
        [self.flip_x, self.flip_y]
    }
}
`);
rep(`#[cfg(test)]
mod tests {`,`#[cfg(test)]
mod tests {
    #[test]
    fn a_flip_by_axis_is_the_field_of_that_name_in_the_order_written() {
        let mut framing = super::Framing::identity();
        framing.flip(super::Axis::X);
        assert!(framing.flip_x && !framing.flip_y);
        assert_eq!(framing.flipped(), [true, false]);
        framing.flip(super::Axis::Y);
        framing.flip(super::Axis::X);
        assert!(!framing.flip_x && framing.flip_y);
        assert_eq!(framing.flipped(), [false, true]);
    }
`);
});
edit('src/command.rs',(rep)=>{
rep(`    /// Mirror the focused camera's picture, and again to put it back. A
    /// latch on a button with a lamp, not a knob: a mirror is on or off.
    Flip(Axis),`,`    /// A latch on a button with a lamp, not a knob: a flip is on or off.
    Flip(Axis),`);
});
edit('src/midi.rs',(rep)=>{
rep(`/// What the lamps say that the focus alone cannot: one fact about the
/// focused monitor, two about the focused camera, and two latched modes of
/// the display. The caller owns every one of them.`,`/// What the lamps say that the focus alone cannot. The caller owns every
/// one of them.`);
rep(`fn the_page_button_is_lit_page_two()`,`fn the_page_button_is_lit_on_page_two()`);
rep(`                    f.knob.why_off(params)`,`                    f.knob.off_because(params).expect("is_on said no")`);
rep(`            why.contains(r#""send" is a level on an input, and this graph has 0"#),`,`            why.contains(r#""send" is a level on an input, and this graph has none"#),`);
rep(`        *framing.mirror(axis) ^= true;`,`        framing.flip(axis);`);
});
edit('README.md',(rep)=>{
rep(`leaves twenty-one select buttons dead`,`leaves nineteen select buttons dead`);
rep(`Every knob is on the
factory map, on one page or the other;`,`Every knob the graph has is on
the factory map, on one page or the other;`);
rep(`and is as far as the delay fader goes. \`framing\``,`and is as far as the delay fader goes; a camera's \`delay\` past it is refused.
\`framing\``);
});
