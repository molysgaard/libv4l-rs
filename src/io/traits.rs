use std::io;

use crate::buffer::Metadata;

/// Streaming I/O
pub trait Stream {
    type Item: ?Sized;

    /// Start streaming, takes exclusive ownership of a device
    fn start(&mut self) -> io::Result<()>;

    /// Stop streaming, frees all buffers
    fn stop(&mut self) -> io::Result<()>;
}

pub trait CaptureStream<'a>: Stream {
    /// Insert a buffer into the drivers' incoming queue
    fn queue(&mut self, index: usize) -> io::Result<()>;

    /// Remove a buffer from the drivers' outgoing queue
    fn dequeue(&mut self) -> io::Result<usize>;

    /// Fetch a new frame by first queueing and then dequeueing.
    /// First time initialization is performed if necessary.
    fn next(&'a mut self) -> io::Result<(&Self::Item, &Metadata)>;
}

pub trait OutputStream<'a>: Stream {
    /// Insert a buffer into the drivers' incoming queue
    fn queue(&mut self, index: usize) -> io::Result<()>;

    /// Remove a buffer from the drivers' outgoing queue
    fn dequeue(&mut self) -> io::Result<usize>;

    /// Dump a new frame by first queueing and then dequeueing.
    /// First time initialization is performed if necessary.
    fn next(&'a mut self) -> io::Result<(&mut Self::Item, &mut Metadata)>;
}

pub struct DevData<T: Sized> {
    /// The index of the device that dequeued the buffer
    pub device: usize,
    /// The data associated with the device
    pub data: T,
}

/// Stream that multiplexes multiple devices
/// Each call to dequeue() returns a tuple of N items and N metadata
pub trait CaptureStreamMulti<'a, const N: usize>: Stream {
    /// Insert a buffer into the drivers' incoming queue
    fn queue(&mut self, index: DevData<usize>) -> io::Result<()>;

    /// Remove a buffer from the drivers' outgoing queue
    fn dequeue(&mut self) -> io::Result<Vec<DevData<usize>>>;

    /// Fetch a new frame from all devices, only return when all devices have a buffer available.
    fn next_all(&'a mut self) -> io::Result<[(&Self::Item, &Metadata); N]>;
}
