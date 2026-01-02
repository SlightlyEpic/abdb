use std::mem::size_of;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{
    common::constants::PAGE_BUF_SIZE,
    page::{UberPageHeader, overlays},
};

pub struct TablePage<T> {
    data: T,
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Header {
    pub num_slots: u16,
    pub data_offset: u16,
    _pad: [u8; 4],
}

#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct Slot {
    pub offset: u16,
    pub length: u16,
}

#[derive(Debug)]
pub enum Error {
    InvalidIndex(usize),
    NoSpace,
    ConvertError(overlays::common::ConvertError),
    Corruption(String),
}

const UBER_HEADER_SIZE: usize = size_of::<UberPageHeader>();
const PAGE_HEADER_SIZE: usize = size_of::<Header>();
const HEADERS_SIZE: usize = UBER_HEADER_SIZE + PAGE_HEADER_SIZE;

impl<T> TablePage<T>
where
    T: AsRef<[u8]>,
{
    pub fn new(data: T) -> Self {
        if data.as_ref().len() != PAGE_BUF_SIZE {
            panic!(
                "TablePage new called with buffer of size {} (expected {})",
                data.as_ref().len(),
                PAGE_BUF_SIZE
            );
        }
        Self { data }
    }

    pub fn uber_header(&self) -> Result<&UberPageHeader, Error> {
        UberPageHeader::ref_from_prefix(self.data.as_ref())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }

    pub fn header(&self) -> Result<&Header, Error> {
        Header::ref_from_prefix(&self.data.as_ref()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(Error::ConvertError)
    }

    pub fn free_space_remaining(&self) -> Result<usize, Error> {
        let header = self.header()?;
        
        let slots_end = HEADERS_SIZE + (header.num_slots as usize * size_of::<Slot>());
        let data_start = header.data_offset as usize;
        
        // If data_start < slots_end, page is corrupted or full
        Ok(data_start.saturating_sub(slots_end))
    }

    pub fn slot_array(&self) -> Result<&[Slot], Error> {
        let header = self.header()?;
        let count = header.num_slots as usize;

        let (physical_slots, _) = <[Slot]>::ref_from_prefix(
            &self.data.as_ref()[HEADERS_SIZE..]
        ).map_err(overlays::common::ConvertError::from)
         .map_err(Error::ConvertError)?;

        physical_slots.get(..count).ok_or(Error::Corruption("num_slots is possibly corrupted".to_owned()))
    }

    pub fn get_slot(&self, idx: usize) -> Result<&Slot, Error> {
        let slots = self.slot_array()?;
        slots.get(idx).ok_or(Error::InvalidIndex(idx))
    }

    pub fn get_data(&self, idx: usize) -> Result<&[u8], Error> {
        let slots = self.slot_array()?;
        let slot = slots.get(idx).ok_or(Error::InvalidIndex(idx))?;
        
        let start = slot.offset as usize;
        let end = start + slot.length as usize;
        
        self.data.as_ref().get(start..end).ok_or(Error::Corruption(format!("slot[{idx}] is possibly corrupted")))
    }
}

impl<T> TablePage<T>
where
    T: AsMut<[u8]> + AsRef<[u8]>,
{
    pub fn uber_header_mut(&mut self) -> Result<&mut UberPageHeader, Error> {
        UberPageHeader::mut_from_prefix(self.data.as_mut())
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }

    pub fn header_mut(&mut self) -> Result<&mut Header, Error> {
        Header::mut_from_prefix(&mut self.data.as_mut()[UBER_HEADER_SIZE..])
            .map(|r| r.0)
            .map_err(overlays::common::ConvertError::from)
            .map_err(|e| Error::ConvertError(e))
    }

    pub fn init(&mut self) -> Result<(), Error> {
        let header = self.header_mut()?;
        header.num_slots = 0;
        header.data_offset = PAGE_BUF_SIZE as u16; 
        Ok(())
    }

    pub fn slot_array_mut(&mut self) -> Result<&mut [Slot], Error> {
        let header = self.header()?;
        let count = header.num_slots as usize;

        let (physical_slots, _) = <[Slot]>::mut_from_prefix(
            &mut self.data.as_mut()[HEADERS_SIZE..]
        ).map_err(overlays::common::ConvertError::from)
         .map_err(Error::ConvertError)?;

        physical_slots.get_mut(..count).ok_or(Error::Corruption("num_slots is possibly corrupted".to_owned()))
    }

    pub fn get_slot_mut(&mut self, idx: usize) -> Result<&mut Slot, Error> {
        let slots = self.slot_array_mut()?;
        slots.get_mut(idx).ok_or(Error::InvalidIndex(idx))
    }

    pub fn get_data_mut(&mut self, idx: usize) -> Result<&mut [u8], Error> {
        let slot = self.get_slot_mut(idx)?;
        
        let start = slot.offset as usize;
        let end = start + slot.length as usize;
        
        self.data.as_mut().get_mut(start..end).ok_or(Error::Corruption(format!("slot[{idx}] is possibly corrupted")))
    }

    /// Returns the index of the inserted tuple.
    pub fn insert(&mut self, tuple_data: &[u8]) -> Result<u16, Error> {
        let needed_space = tuple_data.len() + size_of::<Slot>();
        
        if self.free_space_remaining()? < needed_space {
            return Err(Error::NoSpace);
        }

        let header = self.header_mut()?;
        
        // Write data
        let slot_idx = header.num_slots;
        let new_data_offset = header.data_offset - tuple_data.len() as u16;

        let data_slice = self.data.as_mut();
        data_slice[new_data_offset as usize..(new_data_offset as usize + tuple_data.len())]
            .copy_from_slice(tuple_data);

        // Update header
        let header = self.header_mut()?;
        header.data_offset = new_data_offset;
        header.num_slots += 1;

        // Write slot
        let slot = self.get_slot_mut(slot_idx as usize)?;
        
        slot.offset = new_data_offset;
        slot.length = tuple_data.len() as u16;

        Ok(slot_idx)
    }

    /// Deletes a tuple by marking its length as 0. Can be lazily compacted later.
    /// The reason is that if we actually remove it then all subsequent slot ids will be changed
    /// And any record ids pointing to those will have to be updated, which will be very expensive
    pub fn delete(&mut self, slot_idx: usize) -> Result<(), Error> {
        let header = self.header_mut()?;
        if slot_idx >= header.num_slots as usize {
            return Err(Error::InvalidIndex(slot_idx));
        }

        let slot = self.get_slot_mut(slot_idx)?;
        slot.length = 0; 
        
        Ok(())
    }
}