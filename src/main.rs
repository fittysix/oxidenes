extern crate sdl2;
extern crate time;
extern crate ringbuf;

use sdl2::pixels::PixelFormatEnum;
use sdl2::keyboard::Keycode;
use sdl2::event::Event;
use sdl2::audio::{AudioCallback, AudioSpecDesired};
use std::sync::{Arc, Mutex};
use ringbuf::{RingBuffer, Consumer};
// use std::sync::mpsc::channel;
// use std::sync::mpsc::Receiver;
// use time;

use std::env;
use std::fmt;

mod cart;
mod mem_map;
mod cpu;
mod apu;
mod ppu;
mod joy;
mod opcodes;

use opcodes::AddressMode;

use mem_map::*;
// use cpu::RunCondition;

const PPU_MULTIPLIER:isize = 3;

pub struct Bus {
    ram: Box<[u8]>,
    cart: cart::Cart,
    apu: apu::APU,
    ppu: ppu::PPU,
    joy: joy::Joy,
}

pub struct ApuOut {
    rb: Consumer<f32>,
//    phase: Arc<Mutex<Vec<f32>>>,
//    rx: Receiver<f32>,
}

impl AudioCallback for ApuOut {
    type Channel = f32;
    fn callback(&mut self, out: &mut [f32]) {
        // buffer prevents pops when underflowing the actual output
        // it will make the sound very slgihtly wrong on underflow
        // but it sounds better than a pop
        let mut sample: f32 = 0.0;
        let mut buffer: f32;
        for x in out.iter_mut() {
            buffer = sample;
            sample = if self.rb.is_empty() {
                buffer 
            } else {
                self.rb.pop().unwrap()
            };
   //         sample = self.phase.lock().unwrap().pop().unwrap_or(-1.0);
   //         if sample == -1.0 {sample = buffer};
            *x = sample;

        //    *x = self.rx.try_recv().unwrap_or(0.0);
        }
    }
}

fn main() {
    let rompath = env::args().nth(1).unwrap_or(String::from("smb.nes"));

    let sdl = sdl2::init().unwrap();
    let video = sdl.video().unwrap();
    let window = video.window("OxideNES", 256 * 2, 240 * 2)
        .position_centered()
        .opengl()
        .build()
        .unwrap();

    let mut renderer = window.into_canvas().build().unwrap();
    let t_c = renderer.texture_creator();
    let mut texture = t_c.create_texture_streaming(PixelFormatEnum::RGB24,
                                                        256,
                                                        240).unwrap();
    let mut events = sdl.event_pump().unwrap();

    let audio_subsystem = sdl.audio().unwrap();
    let desired_spec = AudioSpecDesired {
        freq: Some(44100),
        channels: Some(1),  // mono
        samples: Some(441),       // default sample size
    };

    let rb = RingBuffer::<f32>::new(2048);
    let (mut prod, mut cons) = rb.split();
    // let (tx, rx) = channel();



    let cart = cart::Cart::new(&rompath);
    println!("{:#?}", cart);
    let chr_rom = cart::ChrRom::new(&rompath);
    // let apu = apu::APU::new(tx);
    let apu = apu::APU::new(prod);


    let ppu = ppu::PPU::new(chr_rom);
    let joy = joy::Joy::new();

    let cpubus = Bus {
        ram: vec![0; RAM_LEN as usize].into_boxed_slice(),
        cart: cart,
        apu: apu,
        ppu: ppu,
        joy: joy,
    };

    let pc = cpubus.cart.read_cart_u16(RESET_VECTOR_LOC);
    // println!("PC is {:#X}", pc);
    let mut cpu = cpu::CPU::new(cpubus, pc as u16);
    // println!("{:#?}", cpu);
    // let f = Rc::new(&cpu);
    // cpu.bus.apu.setup_read_u8(f);

    let device = audio_subsystem.open_playback(None, &desired_spec, |spec| {
        // Show obtained AudioSpec
        println!("{:?}", spec);

        // initialize the audio callback
        ApuOut {
            rb: cons,
//            phase: cpu.bus.apu.output.clone(),
        //    rx: rx,
        }
    }).unwrap();
    device.resume();
    // println!("{:?}", device);
    // TODO: re-add specific run conditions for debugging
//    let mut nmi = false;
//    let mut irq = false;
    let mut framestart = time::precise_time_ns();
    'main: loop {

        let (op, instr) = cpu.read_instruction();
        if op == 0 {
            println!("BRK, quitting!");
            break;
        }

        // TODO: Move this to a specific debug output
        if false {
            cpu_debug(&op, &instr, &cpu);
        }

        cpu.cycle += instr.ticks as isize * PPU_MULTIPLIER;
        let (nmi, mut irq) = cpu.bus.ppu.tick(instr.ticks as isize * PPU_MULTIPLIER);

        if cpu.bus.ppu.extra_cycle {
            cpu.cycle += 1;
            cpu.bus.ppu.extra_cycle = false;
        }

        if cpu.cycle >= 341 {
            cpu.cycle %= 341;

            if cpu.bus.ppu.scanline == 240 {
                render_frame(&cpu.bus.ppu.screen, &mut renderer, &mut texture);

                // Frame limiter.
                let mut frametime = time::precise_time_ns() - framestart;
                // println!("Frame took {}", frametime);
                if frametime < 16_666_667 {
                    frametime = 16_666_667 - frametime;
                    std::thread::sleep(std::time::Duration::new(0, frametime as u32));
                }
                framestart = time::precise_time_ns();


                for event in events.poll_iter() {
                    match event {
                        Event::Quit {..} | Event::KeyDown { keycode: Some(Keycode::Escape), .. } => {
                            break 'main
                        }
                        _ => ()
                    }
                }

                let keys: Vec<Keycode> = events.
                                keyboard_state().
                                pressed_scancodes().
                                filter_map(Keycode::from_scancode).
                                collect();

                cpu.bus.joy.set_keys(keys);
            }
        }
        irq |= cpu.bus.apu.tick(instr.ticks as isize, &cpu.bus.cart);
//        irq |= cpu.bus.cart.irq_clock(instr.ticks as isize * PPU_MULTIPLIER, cpu.bus.ppu.scanline);

        cpu.execute_op(&op, &instr);

        // TODO: IRQ from apu

        if nmi {
            //    println!("NMI");
            cpu.nmi();
            cpu.bus.ppu.tick(7 * PPU_MULTIPLIER);
            irq |= cpu.bus.apu.tick(7, &cpu.bus.cart);
//            irq |= cpu.bus.cart.irq_clock(7 * PPU_MULTIPLIER, cpu.bus.ppu.scanline);
        }
        if irq {
//            println!("IRQ potentially generated");
            cpu.irq();
        }
    }
}



fn cpu_debug (op: &u8, instr: &opcodes::Instruction, cpu: &cpu::CPU) {
    let pc = cpu.program_counter - instr.bytes as u16;
    let operand = if instr.bytes != 1 {
        // opr = instr.operand.unwrap();
        if instr.operand > 0xFF {
            let opr1 = instr.operand & 0xFF;
            let opr2 = instr.operand >> 8;
            format!("{:02X} {:02X}", opr1 as u8, opr2 as u8)
        } else {
            format!("{:02X}   ", instr.operand as u8)
        }
    } else {
        format!("     ")
    };

    let addrs = if instr.dest_addr != None {
        let addr = instr.dest_addr.unwrap();
        let value = if addr < 0x800 {
            format!(" = {:02X}", cpu.bus.ram[addr as usize])
        } else {
            format!("")
        };
        match instr.addr_mode {
            AddressMode::Immediate => format!("#${:02X}", instr.operand as u8),
            AddressMode::Absolute => format!("${:04X}{}", addr, value),
            AddressMode::AbsoluteX => format!("${:04X},X @ {:04X}{}", instr.operand,
                                                                      addr,
                                                                      value),
            AddressMode::AbsoluteY => format!("${:04X},Y @ {:04X}{}", instr.operand,
                                                                      addr,
                                                                      value),
            AddressMode::XIndirect => {
                format!("(${:02X},X) @ {:02X} = {:04X}{}", instr.operand,
                                                            instr.operand.wrapping_add(cpu.index_x as u16),
                                                            addr,
                                                            value)
            }
            AddressMode::IndirectY => {
                format!("(${:02X}),Y = {:04X} @ {:04X}{}", instr.operand,
                                                            addr.wrapping_sub(cpu.index_y as u16),
                                                            addr,
                                                            value)
            }
            AddressMode::Zeropage => format!("${:02X}{}", addr as u8, value),
            AddressMode::ZeropageX => format!("${:02X},X @ {:02X}{}", instr.operand,
                                                                        addr,
                                                                        value),
            AddressMode::ZeropageY => format!("${:02X},Y @ {:02X}{}", instr.operand,
                                                                        addr,
                                                                        value),
            AddressMode::Indirect => format!("(${:04X}) = {:04X}", instr.operand, addr),
            AddressMode::Relative => format!("${:04X}", (cpu.program_counter as i16 +
                                                        instr.operand as i8 as i16) as u16),
            _ => format!(""),
        }
    } else if instr.addr_mode == AddressMode::Accumulator {
        format!("A")
    } else {
        format!("")
    };
    let tmp: u8 = cpu.status_reg.into();
    print!("{:04X}  {:02X} {} {:>4} {:<27} A:{:02X} X:{:02X} Y:{:02X} P:{:02X} \
              SP:{:02X} CYC:{:>3} SL:{:}\r\n",
             pc,
             op,
             operand,
             instr.name,
             addrs,
             cpu.accumulator,
             cpu.index_x,
             cpu.index_y,
             tmp,
             cpu.stack_pointer,
             cpu.cycle % 341,
             cpu.bus.ppu.scanline,
             ); //, self.status_reg);

}


fn render_frame(screen: &[[u32; 256]; 240],
                renderer: &mut sdl2::render::Canvas<sdl2::video::Window>,
                texture: &mut sdl2::render::Texture,
                // events: &mut sdl2::EventPump,
                )
{
    //println!("Screen 10,10 {:#X}", screen[10][10]);
    texture.with_lock(None, |buffer: &mut [u8], pitch: usize| {
        // println!("pitch is: {:}", pitch);
        for row in 0..240 {
            let offset1 = row * pitch;
            for col in 0..256 {
                let offset2 = col * 3;
                let pixel = screen[row][col];
                let r = (pixel >> 16) as u8;
                let g = ((pixel >> 8) & 0xff) as u8;
                let b = (pixel & 0xff) as u8;

                buffer[offset1 + 0 + offset2] = r;
                buffer[offset1 + 1 + offset2] = g;
                buffer[offset1 + 2 + offset2] = b;

            }
        }
    }).unwrap();

    renderer.clear();
    renderer.copy(&texture, None, None);
    renderer.present();

}








impl fmt::Debug for Bus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "")
    }
}
