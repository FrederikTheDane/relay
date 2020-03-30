use std::net::{TcpListener, TcpStream, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::io::{Write, BufReader, BufRead};
use std::thread;
use std::time;
use std::collections::{HashMap};
use ctrlc;
use clap::{App, Arg};


fn main() {
    let matches = App::new("Relay")
        .version("0.1.0")
        .about("An IPC program built using a client-server structure.")
        .arg(Arg::with_name("port")
             .short("p")
             .long("port")
             .value_name("PORT")
             .help("Set a port to listen on")
             .takes_value(true))
        .get_matches();

    if !matches.is_present("port") {
        println!("Port not specified. Falling back on default port 6060");
    }

    let s_port = matches.value_of("port").unwrap_or("6060");

    s_port.parse::<u16>().unwrap_or_else( |_| panic!("Port {} is not a valid port number", s_port));

    let run = Arc::new(AtomicBool::new(true));
    let r = run.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::Release);
        println!("Received signal");
    }).expect("Error setting interrupt handler");

    //Holds connections for all the clients
    let connections = Arc::new(Mutex::new(HashMap::<SocketAddr, TcpStream>::new()));

    let listener = TcpListener::bind(format!("127.0.0.1:{}", s_port)).expect("Failed to bind listener");
    println!("Listening on port {}", s_port);

    let (snd, rcv) = mpsc::channel::<Vec<u8>>();

    let dist_run = run.clone();

    let connections_dist_copy = connections.clone();
    thread::spawn(move || {
        distribute(rcv, dist_run, connections_dist_copy);
    });

    while listener.set_nonblocking(true).is_err() {}

    while run.load(Ordering::Acquire) {
        let stream = listener.accept();
        if let Ok(stream) = stream {
            println!("Connected to {}", stream.1);
            let run_clone = run.clone();
            let send = snd.clone();
            let mut cloned_stream = stream.0.try_clone().unwrap();
            connections.lock().unwrap().insert(stream.1, stream.0);
            thread::spawn(move || {
                handle_connection(send, run_clone, &mut cloned_stream);
            });        
        }
        thread::sleep(time::Duration::from_millis(1000));
    }
    if !connections.lock().unwrap().is_empty() {
        println!("Some connections are still open. Force shutdown in 5 seconds");
        thread::sleep(time::Duration::from_secs(5));
    }
    
    println!("Server shut down");
}

fn distribute(rec: mpsc::Receiver<Vec<u8>>, should_run: Arc<AtomicBool>, streams: Arc<Mutex<HashMap<SocketAddr, TcpStream>>>) {
    while should_run.load(Ordering::Acquire) {
        let read_vec = rec.recv_timeout(time::Duration::from_millis(10));
        if let Ok(vec) = read_vec {
            println!("{}", std::str::from_utf8(&vec).unwrap());
            match streams.lock() {
                Ok(mut streams_map) => {
                    let mut to_remove = Vec::new();
                    for (a, s) in streams_map.iter_mut() {
                        if let Err(error) = s.write_all(&vec) {
                            println!("Error writing to stream: {:?}\nShutting down stream", error);
                            to_remove.push(a.clone());
                        }
                    }
                    for a in to_remove {
                        streams_map.remove(&a);
                    }
                }
                Err(error) => {
                    println!("Error trying to acquire mutex: {:?}\nAttempting to shut down gracefully", error);
                    return;
                }
            }
        }
    }
}


fn handle_connection(snd: mpsc::Sender<Vec<u8>>, should_run: Arc<AtomicBool>, stream: &mut TcpStream) {
    let buf = &mut Vec::new();
    let mut reader = BufReader::new(stream);
    while should_run.load(Ordering::Acquire) {
        match reader.read_until(0u8, buf) {
            Ok(_) => {
                if let Err(error) = snd.send(buf.to_owned()) {
                    println!("Error sending to channel: {:?}\nReceiver is possibly lost, terminating program", error);
                    should_run.store(false, Ordering::Release);
                }
            }
            Err(error) => {
                match error.kind() {
                    std::io::ErrorKind::WouldBlock => {}
                    _ => {
                        println!("Error reading from the stream: {:?}\nDisconnecting client", error);
                        return;
                    }
                }
            }
        }
    }
}
