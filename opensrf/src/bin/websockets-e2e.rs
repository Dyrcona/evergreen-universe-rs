use opensrf::message;
use opensrf::util;
use std::thread;
use std::time::{Instant, Duration};
use std::io::Write;
use websocket::stream::sync::NetworkStream;
use websocket::sync::Client;
use websocket::{ClientBuilder, Message, OwnedMessage};

/// Each websocket client will send this many requests in a loop.
const REQS_PER_THREAD: usize = 100;

/// Number of parallel websocket clients to launch.
/// Be cautious when setting this value, especially on a production
/// system, since it's trivial to overwhelm a service with too many
/// websocket clients making API calls to the same service.
const THREAD_COUNT: usize = 10;

/// Websocket server URI.
//const DEFAULT_URI: &str = "wss://redis.demo.kclseg.org:443/osrf-websocket-translator";
const DEFAULT_URI: &str = "ws://127.0.0.1:7682";

/// How many times we repeat the entire batch.
const NUM_ITERS: usize = 5;

/// If non-zero, have each thread pause this many ms between requests.
/// Helpful for focusing on endurance / real-world traffic patterns more
/// than per-request speed.
const REQ_PAUSE: u64 = 10;
//const REQ_PAUSE: u64 = 0;

// Since we're testing Websockets, which is a public-facing gateway,
// the destination service must be a public service.
const SERVICE: &str = "open-ils.actor";

fn main() {
    let mut batches = 0;
    let reqs_per_batch = THREAD_COUNT * REQS_PER_THREAD;

    while batches < NUM_ITERS {
        batches += 1;
        let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();

        let start = Instant::now();

        while handles.len() < THREAD_COUNT {
            handles.push(thread::spawn(|| run_thread()));
        }

        // Wait for all threads to finish.
        for h in handles {
            h.join().ok();
        }

        let duration = (start.elapsed().as_millis() as f64) / 1000.0;
        println!(
            "\n\nBatch Requests: {reqs_per_batch}; Duration: {:.3}\n",
            duration
        );
    }

    println!("Total requests processed: {}", reqs_per_batch * NUM_ITERS);
}

fn run_thread() {

    // TODO: At present, dummy SSL certs will fail.
    // https://docs.rs/websocket/latest/websocket/client/builder/struct.ClientBuilder.html#method.connect
    // https://docs.rs/native-tls/0.2.8/native_tls/struct.TlsConnectorBuilder.html
    let mut client = ClientBuilder::new(DEFAULT_URI)
        .unwrap()
        .connect(None)
        .unwrap();

    let mut counter = 0;

    while counter < REQS_PER_THREAD {
        send_one_request(&mut client, counter);
        counter += 1;
        if REQ_PAUSE > 0 {
            thread::sleep(Duration::from_millis(REQ_PAUSE));
        }
    }
}

fn send_one_request(client: &mut Client<Box<dyn NetworkStream + Send>>, count: usize) {
    let echo = format!("Hello, World {count}");
    let echostr = echo.as_str();

    let message = json::object! {
        thread: util::random_number(12),
        service: SERVICE,
        osrf_msg: [{
            __c: "osrfMessage",
            __p: {
                threadTrace:1,
                type: "REQUEST",
                locale: "en-US",
                timezone: "America/New_York",
                api_level: 1,
                ingress: "opensrf",
                payload:{
                    __c: "osrfMethod",
                    __p:{
                        method: "opensrf.system.echo",
                        params: [echostr],
                    }
                }
            }
        }]
    };

    if let Err(e) = client.send_message(&Message::text(message.dump())) {
        eprintln!("Error in send: {e}");
        return;
    }

    let response = match client.recv_message() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error in recv: {e}");
            return;
        }
    };

    if let OwnedMessage::Text(text) = response {
        let mut ws_msg = json::parse(&text).unwrap();
        let mut osrf_list = ws_msg["osrf_msg"].take();
        let osrf_msg = osrf_list[0].take();

        if osrf_msg.is_null() {
            panic!("No response from request");
        }

        let msg = message::Message::from_json_value(osrf_msg).unwrap();

        if let message::Payload::Result(res) = msg.payload() {
            let content = res.content();
            assert_eq!(content, &echostr);
            print!("+");
            std::io::stdout().flush().ok();
        }
    }
}