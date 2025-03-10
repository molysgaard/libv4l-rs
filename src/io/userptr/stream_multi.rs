use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::convert::TryInto;
use std::os::fd::BorrowedFd;
use std::time::Duration;
use std::{io, mem, sync::Arc};

use allocator_api2::alloc::Allocator;

use crate::buffer::{Metadata, Type};
use crate::device::{Device, Handle};
use crate::io::traits::{CaptureStreamMulti, DevData, Stream as StreamTrait};
use crate::io::userptr::arena::Arena;
use crate::memory::Memory;
use crate::v4l2;
use crate::v4l_sys::*;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

/// Stream of user buffers for multiple devices
///
/// An arena instance is used internally for buffer handling for each device.
pub struct AllocStream<A: Allocator + Clone, const N: usize> {
    handles: [Arc<Handle>; N],
    arenas: [Arena<A>; N],
    arena_indices: [usize; N],
    buf_types: [Type; N],
    buf_metas: Vec<Vec<Metadata>>,
    timeout: Option<i32>,
    active: bool,
}

pub type Stream<const N: usize> = AllocStream<allocator_api2::alloc::Global, N>;

impl<const N: usize> Stream<N> {
    pub fn new(devs: [&Device; N], buf_types: [Type; N]) -> io::Result<Self> {
        Stream::with_buffers(devs, buf_types, [4; N])
    }

    pub fn with_buffers(
        devs: [&Device; N],
        buf_types: [Type; N],
        buf_counts: [u32; N],
    ) -> io::Result<Self> {
        Self::with_buffers_and_alloc(devs, buf_types, buf_counts, allocator_api2::alloc::Global)
    }
}

impl<A: Allocator + Clone, const N: usize> AllocStream<A, N> {
    pub fn with_buffers_and_alloc(
        devs: [&Device; N],
        buf_types: [Type; N],
        buf_counts: [u32; N],
        allocator: A,
    ) -> io::Result<Self> {
        let handles: [Arc<Handle>; N] = core::array::from_fn(|i| devs[i].handle());
        let mut arenas: [Arena<A>; N] = core::array::from_fn(|i| {
            Arena::with_alloc(handles[i].clone(), buf_types[i], allocator.clone())
        });
        let mut buf_metas = Vec::with_capacity(N);

        for i in 0..N {
            let count = arenas[i].allocate(buf_counts[i])?;
            let meta = vec![Metadata::default(); count as usize];
            buf_metas.push(meta);
        }

        Ok(AllocStream {
            handles,
            arenas,
            arena_indices: [0; N],
            buf_types,
            buf_metas,
            active: false,
            timeout: None,
        })
    }

    pub fn handle(&self) -> [Arc<Handle>; N] {
        self.handles.clone()
    }

    pub fn set_timeout(&mut self, duration: Duration) {
        self.timeout = Some(duration.as_millis().try_into().unwrap());
    }

    pub fn clear_timeout(&mut self) {
        self.timeout = None;
    }
}

impl<A: Allocator + Clone, const N: usize> Drop for AllocStream<A, N> {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            if let Some(code) = e.raw_os_error() {
                // ENODEV means the file descriptor wrapped in the handle became invalid, most
                // likely because the device was unplugged or the connection (USB, PCI, ..)
                // broke down. Handle this case gracefully by ignoring it.
                if code == 19 {
                    /* ignore ENODEV */
                    return;
                }
            }
            panic!("{:?}", e)
        }
    }
}

impl<A: Allocator + Clone, const N: usize> StreamTrait for AllocStream<A, N> {
    type Item = [u8];

    fn start(&mut self) -> io::Result<()> {
        // TODO: Currently, this will fail if we do not queue anything before calling start().
        // At least with the poll implementation, we will gett POLLERR back.
        // Doing it this way just mirrors other implementations, but I think it's not
        // the best way to handle this.
        // See beginning of CaptureStreamMulti::next_all() for more details.
        for i in 0..N {
            unsafe {
                let mut typ = self.buf_types[i] as u32;
                v4l2::ioctl(
                    self.handles[i].fd(),
                    v4l2::vidioc::VIDIOC_STREAMON,
                    &mut typ as *mut _ as *mut std::os::raw::c_void,
                    self.handles[i].use_libc(),
                )?;
            }
        }

        self.active = true;
        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        for i in 0..N {
            unsafe {
                let mut typ = self.buf_types[i] as u32;
                v4l2::ioctl(
                    self.handles[i].fd(),
                    v4l2::vidioc::VIDIOC_STREAMOFF,
                    &mut typ as *mut _ as *mut std::os::raw::c_void,
                    self.handles[i].use_libc(),
                )?;
            }
        }

        self.active = false;
        Ok(())
    }
}

impl<'a, A: Allocator + Clone, const N: usize> CaptureStreamMulti<'a, N> for AllocStream<A, N> {
    fn queue(&mut self, index: DevData<usize>) -> io::Result<()> {
        let devidx = index.device;
        let buf = &mut self.arenas[devidx].bufs[index.data];
        let mut v4l2_buf = v4l2_buffer {
            index: index.data as u32,
            type_: self.buf_types[devidx] as u32,
            memory: Memory::UserPtr as u32,
            m: v4l2_buffer__bindgen_ty_1 {
                userptr: buf.as_ptr() as std::os::raw::c_ulong,
            },
            length: buf.len() as u32,
            ..unsafe { mem::zeroed() }
        };

        unsafe {
            v4l2::ioctl(
                self.handles[devidx].fd(),
                v4l2::vidioc::VIDIOC_QBUF,
                &mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
                self.handles[devidx].use_libc(),
            )?;
        }

        Ok(())
    }

    fn dequeue(&mut self) -> io::Result<Vec<DevData<usize>>> {
        let mut poll_fds: Vec<PollFd> = self
            .handles
            .iter()
            .map(|handle| {
                PollFd::new(
                    unsafe { BorrowedFd::borrow_raw(handle.fd()) },
                    PollFlags::POLLIN,
                )
            })
            .collect();

        let timeout = self
            .timeout
            .map(|t| t.try_into().unwrap())
            .unwrap_or(PollTimeout::NONE);
        let ready_count = poll(&mut poll_fds, timeout).unwrap();

        if ready_count == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "VIDIOC_DQBUF"));
        }

        let mut devdata = Vec::with_capacity(ready_count as usize);

        for (devidx, pfd) in poll_fds.iter().enumerate() {
            let events = pfd.revents().unwrap();
            if events.contains(PollFlags::POLLIN) {
                let mut v4l2_buf = v4l2_buffer {
                    type_: self.buf_types[devidx] as u32,
                    memory: Memory::UserPtr as u32,
                    ..unsafe { mem::zeroed() }
                };

                unsafe {
                    v4l2::ioctl(
                        self.handles[devidx].fd(),
                        v4l2::vidioc::VIDIOC_DQBUF,
                        &mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
                        self.handles[devidx].use_libc(),
                    )
                    .unwrap();
                }

                self.arena_indices[devidx] = v4l2_buf.index as usize;
                self.buf_metas[devidx][self.arena_indices[devidx]] = Metadata {
                    bytesused: v4l2_buf.bytesused,
                    flags: v4l2_buf.flags.into(),
                    field: v4l2_buf.field,
                    timestamp: v4l2_buf.timestamp.into(),
                    sequence: v4l2_buf.sequence,
                };

                devdata.push(DevData {
                    device: devidx,
                    data: self.arena_indices[devidx],
                });
            }
        }

        Ok(devdata)
    }

    fn next_all(&'a mut self) -> io::Result<[(&'a Self::Item, &'a Metadata); N]> {
        if !self.active {
            // TODO: I think it makes more sense to queue all buffers on CaptureStreamMulti::start(), but
            // other implementations do it this way so I'll keep it for now.
            // Currently, if you first call start(), and then call next_all() I get POLLERR errors back from poll().
            for device in 0..N {
                for index in 0..self.arenas[device].bufs.len() {
                    self.queue(DevData {
                        device,
                        data: index,
                    })
                    .unwrap();
                }
            }
            self.start()?;
        } else {
            for device in 0..N {
                self.queue(DevData {
                    device,
                    data: self.arena_indices[device],
                })
                .unwrap();
            }
        }

        let mut ready_map: HashMap<usize, usize> = HashMap::new();
        while ready_map.len() < N {
            let dequeued = self.dequeue().unwrap();
            for dequeued in dequeued.iter() {
                let ent = ready_map.entry(dequeued.device);
                match ent {
                    Entry::Occupied(mut o) => {
                        self.queue(DevData {
                            device: *o.key(),
                            data: *o.get(),
                        })
                        .unwrap();
                        log::warn!(
                            "CaptureStreamMulti::next_all() re-queued buffer for device {}. If this happens frequently, something is wrong with the camera system.",
                            dequeued.device
                        );
                        o.insert(dequeued.data);
                    }
                    Entry::Vacant(v) => {
                        v.insert(dequeued.data);
                    }
                }
            }
        }

        let mut results: [Option<(&[u8], &Metadata)>; N] = [None; N];
        for (device, index) in ready_map.iter() {
            results[*device] = Some((
                &self.arenas[*device].bufs[*index],
                &self.buf_metas[*device][*index],
            ));
        }

        Ok(results.map(|r| r.unwrap()))
    }
}
