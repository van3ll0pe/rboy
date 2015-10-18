use blip_buf::BlipBuf;
use cpal;
use std;

macro_rules! try_opt {
     ( $expr:expr ) => {
         match $expr {
             Some(v) => v,
             None => return None,
         }
     }
}

const WAVE_PATTERN : [[i32; 8]; 4] = [[-1,-1,-1,-1,1,-1,-1,-1],[-1,-1,-1,-1,1,1,-1,-1],[-1,-1,1,1,1,1,-1,-1],[1,1,1,1,-1,-1,1,1]];
const CLOCKS_PER_SECOND : u32 = 1 << 22;

struct VolumeEnvelope {
    period : u8,
    goes_up : bool,
    delay : u8,
    initial_volume : u8,
    volume : u8,
}

impl VolumeEnvelope {
    fn new() -> VolumeEnvelope {
        VolumeEnvelope {
            period: 0,
            goes_up: false,
            delay: 0,
            initial_volume: 0,
            volume: 0,
        }
    }

    fn wb(&mut self, a: u16, v: u8) {
        match a {
            0xFF12 | 0xFF17 | 0xFF21 => {
                self.period = v & 0x7;
                self.goes_up = v & 0x8 == 0x8;
                self.initial_volume = v >> 4;
                self.volume = self.initial_volume;
            },
            0xFF14 | 0xFF19 | 0xFF23 if v & 0x80 == 0x80 => {
                self.delay = self.period;
                self.volume = self.initial_volume;
                // enabled = true
            },
            _ => (),
        }
    }

    fn step(&mut self) {
        if self.delay > 1 {
            self.delay -= 1;
        }
        else if self.delay == 1 {
            self.delay = self.period;
            if self.goes_up && self.volume < 15 {
                self.volume += 1;
            }
            else if !self.goes_up && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
}

struct SquareChannel {
    enabled : bool,
    duty : u8,
    phase : u8,
    length: u8,
    length_enabled : bool,
    frequency: u16,
    period: u32,
    last_amp: i32,
    delay: u32,
    has_sweep : bool,
    sweep_frequency: u16,
    sweep_delay: u8,
    sweep_period: u8,
    sweep_shift: u8,
    sweep_by_adding: bool,
    volume_envelope: VolumeEnvelope,
    blip: BlipBuf,
}

impl SquareChannel {
    fn new(blip: BlipBuf, with_sweep: bool) -> SquareChannel {
        SquareChannel {
            enabled: false,
            duty: 1,
            phase: 1,
            length: 0,
            length_enabled: false,
            frequency: 0,
            period: 0,
            last_amp: 0,
            delay: 0,
            has_sweep: with_sweep,
            sweep_frequency: 0,
            sweep_delay: 0,
            sweep_period: 0,
            sweep_shift: 0,
            sweep_by_adding: false,
            volume_envelope: VolumeEnvelope::new(),
            blip: blip,
        }
    }

    fn on(&self) -> bool {
        self.enabled && (!self.length_enabled || self.length < 64)
    }

    fn wb(&mut self, a: u16, v: u8) {
        match a {
            0xFF10 if self.has_sweep => {
                self.sweep_period = (v >> 4) & 0x7;
                self.sweep_shift = v & 0x7;
                self.sweep_by_adding = v & 0x8 == 0x8;
            },
            0xFF11 | 0xFF16 => {
                self.duty = v >> 6;
                self.length = v & 0b0011_1111;
            },
            0xFF13 | 0xFF18 => {
                self.frequency = (self.frequency & 0xFF00) | (v as u16);
                self.calculate_period();
            },
            0xFF14 | 0xFF19 => {
                self.frequency = (self.frequency & 0x00FF) | (((v & 0b0000_0111) as u16) << 8);
                self.calculate_period();
                self.length_enabled = v & 0x40 == 0x40;
                self.enabled = v & 0x80 == 0x80;
                self.delay = 0;

                self.sweep_frequency = self.frequency;
			    if self.has_sweep && self.sweep_period > 0 && self.sweep_shift > 0 {
				    self.sweep_delay = 1;
				    self.step_sweep();
			    }
            },
            _ => (),
        }
        self.volume_envelope.wb(a, v);
    }

    fn calculate_period(&mut self) {
        if self.frequency > 2048 { self.period = 0; }
        else { self.period = (2048 - self.frequency as u32) * 4; }
    }

    // This assumes no volume or sweep adjustments need to be done in the meantime
    fn run(&mut self, start_time: u32, end_time: u32) {
        if !self.enabled || (self.length == 64 && self.length_enabled) || self.period == 0 {
            if self.last_amp != 0 {
                self.blip.add_delta(start_time, -self.last_amp);
                self.last_amp = 0;
                self.delay = 0;
            }
        }
        else {
            let mut time = start_time + self.delay;
            let pattern = WAVE_PATTERN[self.duty as usize];
            let vol = self.volume_envelope.volume;
            while time <= end_time {
                let amp = vol as i32 * pattern[self.phase as usize];
                if amp != self.last_amp {
                    self.blip.add_delta(time, amp - self.last_amp);
                    self.last_amp = amp;
                }
                time += self.period;
                self.phase = (self.phase + 1) % 8;
            }

            // next time, we have to wait an additional delay timesteps
            self.delay = time - end_time;
        }
    }

    fn step_length(&mut self) {
        if self.length_enabled && self.length < 64 {
            self.length += 1;
        }
    }

    fn step_sweep(&mut self) {
        if !self.has_sweep || self.sweep_period == 0 { return; }

        if self.sweep_delay > 1 {
            self.sweep_delay -= 1;
        }
        else {
            self.sweep_delay = self.sweep_period;
            self.frequency = self.sweep_frequency;
            self.calculate_period();

            let offset = self.sweep_frequency >> self.sweep_shift;
            if self.sweep_by_adding {
                if self.sweep_frequency >= 2048 - offset {
                    self.sweep_delay = 0;
                    self.sweep_frequency = 2048;
                }
                else {
                    self.sweep_frequency += offset;
                }
            }
            else {
                if self.sweep_frequency <= offset {
                    self.sweep_frequency = 0;
                }
                else {
                    self.sweep_frequency -= offset;
                }
            }
        }
    }
}

pub struct Sound {
    on: bool,
    registerdata: [u8; 0x17],
    time: u32,
    prev_time: u32,
    next_time: u32,
    time_divider: u8,
    channel1: SquareChannel,
    channel2: SquareChannel,
    volume_left: u8,
    volume_right: u8,
    voice: cpal::Voice,
}

impl Sound {
    pub fn new() -> Option<Sound> {
        let voice = match get_channel() {
            Some(v) => v,
            None => {
                println!("Could not open audio device");
                return None;
            },
        };

        let blipbuf1 = create_blipbuf(&voice);
        let blipbuf2 = create_blipbuf(&voice);

        Some(Sound {
            on: false,
            registerdata: [0; 0x17],
            time: 0,
            prev_time: 0,
            next_time: CLOCKS_PER_SECOND / 256,
            time_divider: 0,
            channel1: SquareChannel::new(blipbuf1, true),
            channel2: SquareChannel::new(blipbuf2, false),
            volume_left: 7,
            volume_right: 7,
            voice: voice,
        })
    }

   pub fn rb(&mut self, a: u16) -> u8 {
        self.run();
        match a {
            0xFF10 ... 0xFF25 => self.registerdata[a as usize - 0xFF10],
            0xFF26 => {
                self.registerdata[a as usize - 0xFF10] & 0xF0
                    | (if self.channel1.on() { 1 } else { 0 })
                    | (if self.channel2.on() { 2 } else { 0 })
            }

            _ => 0,
        }
    }

    pub fn wb(&mut self, a: u16, v: u8) {
        if a != 0xFF26 && !self.on { return; }
        self.run();
        if a >= 0xFF10 && a <= 0xFF26 {
            self.registerdata[a as usize - 0xFF10] = v;
        }
        match a {
            0xFF10 ... 0xFF14 => self.channel1.wb(a, v),
            0xFF16 ... 0xFF19 => self.channel2.wb(a, v),
            0xFF24 => {
                self.volume_left = v & 0x7;
                self.volume_right = (v >> 4) & 0x7;
            }
            0xFF26 => self.on = v & 0x80 == 0x80,
            // 0xFF30 ... 0xFF3F => {
            //     let wave_a = a as usize - 0xFF30;
            //     self.waveram[wave_a * 2] = v >> 4;
            //     self.waveram[wave_a * 2 + 1] = v & 0xF;
            // },
            _ => (),
        }
    }

    pub fn do_cycle(&mut self, cycles: u32)
    {
        if !self.on { return; }

        self.time += cycles;
    }

    pub fn do_output(&mut self) {
        if self.time >= self.voice.get_period() as u32 {
            self.run();
            self.channel1.blip.end_frame(self.prev_time);
            self.channel2.blip.end_frame(self.prev_time);
            self.time -= self.prev_time;
            self.next_time -= self.prev_time;
            self.prev_time = 0;
            self.mix_buffers();
        }
    }

    fn run(&mut self) {
        while self.next_time <= self.time {
            self.channel1.run(self.prev_time, self.next_time);
            self.channel2.run(self.prev_time, self.next_time);

            self.channel1.step_length();
            self.channel2.step_length();

            if self.time_divider == 0 {
                self.channel1.volume_envelope.step();
                self.channel2.volume_envelope.step();
            }
            else if self.time_divider & 1 == 1 {
                self.channel1.step_sweep();
            }

            self.time_divider = (self.time_divider + 1) % 4;
            self.prev_time = self.next_time;
            self.next_time += CLOCKS_PER_SECOND / 256;
        }
    }

    fn active_channels(&self, right: bool) -> i32 {
        let shift = if right { 4 } else { 0 };
        let channels = (self.registerdata[0x15] >> shift) & 0x0F;
        let mut answer = 0;
        if channels & 1 != 0 && self.channel1.on() { answer += 1; }
        if channels & 2 != 0 && self.channel2.on() { answer += 1; }
        //if channels & 4 != 0 && self.channel3.on() { answer += 1; }
        //if channels & 8 != 0 && self.channel4.on() { answer += 1; }
        answer
    }

    fn mix_buffers(&mut self) {
        use std::cmp;

        let maxsize = cmp::min(self.channel1.blip.samples_avail(), self.channel2.blip.samples_avail()) as usize;
        let mut outputted = 0;

        let left_vol = (1.0 / self.active_channels(false) as f32) * (self.volume_left as f32 / 7.0) * (1.0 / 15.0) * 0.5;
        let right_vol = (1.0 / self.active_channels(true) as f32) * (self.volume_right as f32 / 7.0) * (1.0 / 15.0) * 0.5;

        while outputted < maxsize {
            let buf_left = &mut [0f32; 2048];
            let buf_right = &mut [0f32; 2048];
            let buf1 = &mut [0i16; 2048];
            let buf2 = &mut [0i16; 2048];

            let count1 = self.channel1.blip.read_samples(buf1, false);
            for (i, v) in buf1[..count1].iter().enumerate() {
                if self.registerdata[0x15] & 0x01 == 0x01 {
                    buf_left[i] += *v as f32 * left_vol;
                }
                if self.registerdata[0x15] & 0x10 == 0x10 {
                    buf_right[i] += *v as f32 * right_vol;
                }
            }

            let count2 = self.channel2.blip.read_samples(buf2, false);
            for (i, v) in buf2[..count2].iter().enumerate() {
                if self.registerdata[0x15] & 0x02 == 0x02 {
                    buf_left[i] += *v as f32 * left_vol;
                }
                if self.registerdata[0x15] & 0x20 == 0x20 {
                    buf_right[i] += *v as f32 * right_vol;
                }
            }

            debug_assert!(count1 == count2);

            play_buf(&mut self.voice, &buf_left[..count1], &buf_right[..count1]);

            outputted += count1;
        }
    }
}

fn play_buf(voice: &mut cpal::Voice, buf_left: &[f32], buf_right: &[f32]) {
    debug_assert!(buf_left.len() == buf_right.len());

    let left_idx = voice.format().channels.iter().position(|c| *c == cpal::ChannelPosition::FrontLeft);
    let right_idx = voice.format().channels.iter().position(|c| *c == cpal::ChannelPosition::FrontRight);

    let channel_count = voice.format().channels.len();

    let count = buf_left.len();
    let mut done = 0;
    let mut lastdone = count;

    while lastdone != done && done < count {
        lastdone = done;
        let buf_left_next = &buf_left[done..];
        let buf_right_next = &buf_right[done..];
        match voice.append_data(count - done) {
            cpal::UnknownTypeBuffer::U16(mut buffer) => {
                for (i, sample) in buffer.chunks_mut(channel_count).enumerate() {
                    if let Some(idx) = left_idx {
                        sample[idx] = (buf_left_next[i] * (std::i16::MAX as f32) + (std::i16::MAX as f32)) as u16;
                    }
                    if let Some(idx) = right_idx {
                        sample[idx] = (buf_right_next[i] * (std::i16::MAX as f32) + (std::i16::MAX as f32)) as u16;
                    }
                    done += 1;
                }
            }
            cpal::UnknownTypeBuffer::I16(mut buffer) => {
                for (i, sample) in buffer.chunks_mut(channel_count).enumerate() {
                    if let Some(idx) = left_idx {
                        sample[idx] = (buf_left_next[i] * std::i16::MAX as f32) as i16;
                    }
                    if let Some(idx) = right_idx {
                        sample[idx] = (buf_right_next[i] * std::i16::MAX as f32) as i16;
                    }
                    done += 1;
                }
            }
            cpal::UnknownTypeBuffer::F32(mut buffer) => {
                for (i, sample) in buffer.chunks_mut(channel_count).enumerate() {
                    if let Some(idx) = left_idx {
                        sample[idx] = buf_left_next[i];
                    }
                    if let Some(idx) = right_idx {
                        sample[idx] = buf_right_next[i];
                    }
                    done += 1;
                }
            }
        }
    }
    voice.play();
}

fn get_channel() -> Option<cpal::Voice> {
    if cpal::get_endpoints_list().count() == 0 { return None; }

    let endpoint = try_opt!(cpal::get_default_endpoint());
    let format = try_opt!(endpoint.get_supported_formats_list().ok().and_then(|mut v| v.next()));

    cpal::Voice::new(&endpoint, &format).ok()
}

fn create_blipbuf(voice: &cpal::Voice) -> BlipBuf {
    let mut blipbuf = BlipBuf::new(voice.format().samples_rate.0);
    blipbuf.set_rates(CLOCKS_PER_SECOND as f64, voice.format().samples_rate.0 as f64);
    blipbuf
}
