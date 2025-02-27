#[cfg(target_os = "hermit")]
use hermit as _;

use std::net::UdpSocket;

// demo program to test the udp interface
//
// Use `socat - UDP:localhost:9975` to communicate with the
// unikernel.

fn main() {
	let socket = UdpSocket::bind("0.0.0.0:9975").expect("couldn't bind to address");
	let mut buf = [0; 1000];

	loop {
		// Receives a single datagram message on the socket.
		// If `buf` is too small to hold, the message, it will be cut off.
		println!("about to recv");
		match socket.recv(&mut buf) {
			Ok(received) => {
				let msg = std::str::from_utf8(&buf[..received]).unwrap();
				print!("received {}", msg);
				if msg.starts_with("exit") {
					break;
				}
			}
			Err(e) => {
				println!("recv function failed: {e:?}");
				break;
			}
		}
	}
}
