use std::net::{TcpListener, TcpStream, SocketAddr, Shutdown};
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

    //Keeping track of whether the program should keep running, to allow a graceful shutdown
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

    //Channels for sending between a connection handler and the server message distributor
    let (snd, rcv) = mpsc::channel::<Vec<u8>>();

    let dist_run = run.clone();

    //Thread for distributing messages to connected clients
    let connections_dist_clone = connections.clone();
    thread::spawn(move || {
        distribute(rcv, dist_run, connections_dist_clone);
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
            let conn_handler_clone = connections.clone();

            //Start a new thread handling incoming data for the newly connected client
            thread::spawn(move || {
                handle_connection(send, run_clone, &mut cloned_stream, conn_handler_clone);
            });        
        }
        thread::sleep(time::Duration::from_millis(1000));
    }
    let conn = connections.lock().unwrap();
    if !conn.is_empty() {
        println!("{} connection(s) are still open. Force shutdown in 5 seconds", conn.len());
        thread::sleep(time::Duration::from_secs(5));
    }
    
    println!("Server shut down");
}

fn distribute(rec: mpsc::Receiver<Vec<u8>>, should_run: Arc<AtomicBool>, streams: Arc<Mutex<HashMap<SocketAddr, TcpStream>>>) {
    while should_run.load(Ordering::Acquire) {
        let read_vec = rec.recv();
        match read_vec {
            Ok(vec) => {
                //Print the received message to stdout
                println!("{}", std::str::from_utf8(&vec).unwrap());
                match streams.lock() {
                    Ok(mut streams_map) => {
                        let mut to_remove = Vec::new();

                        //Send the message to all clients, and make sure to disconnect the client if something goes wrong
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
            Err(error) => {
                println!("Receiver dropped {:?}\nShutting down program", error);
            }
        }
    }
}


fn handle_connection(snd: mpsc::Sender<Vec<u8>>, should_run: Arc<AtomicBool>, stream: &mut TcpStream, streams: Arc<Mutex<HashMap<SocketAddr, TcpStream>>>) {
    let buf = &mut Vec::new();
    let cloned = stream.try_clone().unwrap();
    let addr = stream.peer_addr().unwrap();
    let mut reader = BufReader::new(stream);
    let mut should_dc = false;
    while should_run.load(Ordering::Acquire) {
        //Read until a null terminator is received
        match reader.read_until(0u8, buf) {
            Ok(bytes_read) => {
                //Make sure we're only sending the message if something was actually received
                if bytes_read == 0 {
                    continue;
                }
                if let Err(error) = snd.send(buf.to_owned()) {
                    println!("Error sending to channel: {:?}\nReceiver is possibly lost, terminating program", error);
                    should_run.store(false, Ordering::Release);
                }
            }
            Err(error) => {
                match error.kind() {
                    std::io::ErrorKind::WouldBlock => {}
                    std::io::ErrorKind::ConnectionAborted => {
                        println!("{} disconnected", addr);
                        should_dc = true;
                    }
                    _ => {
                        println!("Error reading from the stream: {:?}\nDisconnecting client", error);
                        should_dc = true;
                    }
                }
            }
        }
        buf.clear();
        if should_dc {
            streams.lock().unwrap().remove(&addr);
            cloned.shutdown(Shutdown::Both).unwrap();
            return;
        }
    }
}
