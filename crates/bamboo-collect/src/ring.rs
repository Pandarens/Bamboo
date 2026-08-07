//! Кольцевой буфер фиксированной ёмкости.
//!
//! Хранилище уровня L0 из раздела 8.2 ТЗ: разрешение 1 с, глубина 10 минут,
//! всё в памяти. Ёмкость задаётся в точках, а не в секундах, потому что
//! частота опроса плавает от 1 до 60 с.

/// Кольцевой буфер, переписывающий самые старые значения.
///
/// Память выделяется по мере заполнения, а не сразу на всю ёмкость:
/// процессов на машине сотни, а живут большинство из них секунды,
/// и отдавать каждому массив на 600 элементов авансом слишком дорого.
#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    items: Vec<T>,
    capacity: usize,
    /// Куда писать следующее значение. Осмысленно только после заполнения.
    next: usize,
}

impl<T: Copy> RingBuffer<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(
            capacity > 0,
            "ёмкость кольцевого буфера должна быть больше нуля"
        );
        RingBuffer {
            items: Vec::new(),
            capacity,
            next: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.items.len() == self.capacity
    }

    pub fn push(&mut self, value: T) {
        if self.items.len() < self.capacity {
            self.items.push(value);
            self.next = self.items.len() % self.capacity;
        } else {
            self.items[self.next] = value;
            self.next = (self.next + 1) % self.capacity;
        }
    }

    /// Перебор от самого старого значения к самому свежему.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        let split = if self.is_full() { self.next } else { 0 };
        self.items[split..].iter().chain(self.items[..split].iter())
    }

    /// Последнее добавленное значение.
    pub fn last(&self) -> Option<&T> {
        if self.items.is_empty() {
            return None;
        }
        let index = if self.is_full() {
            (self.next + self.capacity - 1) % self.capacity
        } else {
            self.items.len() - 1
        };
        self.items.get(index)
    }

    /// Самое старое сохранившееся значение.
    pub fn oldest(&self) -> Option<&T> {
        if self.items.is_empty() {
            return None;
        }
        if self.is_full() {
            self.items.get(self.next)
        } else {
            self.items.first()
        }
    }

    /// Сколько байт занято под данные. Нужно для контроля бюджета памяти.
    pub fn allocated_bytes(&self) -> usize {
        self.items.capacity() * core::mem::size_of::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(ring: &RingBuffer<u32>) -> Vec<u32> {
        ring.iter().copied().collect()
    }

    #[test]
    fn keeps_order_until_full() {
        let mut ring = RingBuffer::with_capacity(4);
        for value in 1..=3 {
            ring.push(value);
        }
        assert_eq!(collect(&ring), vec![1, 2, 3]);
        assert_eq!(ring.last(), Some(&3));
        assert_eq!(ring.oldest(), Some(&1));
        assert!(!ring.is_full());
    }

    #[test]
    fn overwrites_oldest_when_full() {
        let mut ring = RingBuffer::with_capacity(4);
        for value in 1..=6 {
            ring.push(value);
        }
        assert!(ring.is_full());
        assert_eq!(collect(&ring), vec![3, 4, 5, 6]);
        assert_eq!(ring.oldest(), Some(&3));
        assert_eq!(ring.last(), Some(&6));
    }

    #[test]
    fn length_never_exceeds_capacity() {
        let mut ring = RingBuffer::with_capacity(3);
        for value in 0..1000 {
            ring.push(value);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(collect(&ring), vec![997, 998, 999]);
    }

    #[test]
    fn memory_is_allocated_lazily() {
        let ring: RingBuffer<u64> = RingBuffer::with_capacity(600);
        assert_eq!(
            ring.allocated_bytes(),
            0,
            "пустой буфер не должен занимать память"
        );
    }

    #[test]
    fn capacity_one_works() {
        let mut ring = RingBuffer::with_capacity(1);
        ring.push(10);
        ring.push(20);
        assert_eq!(collect(&ring), vec![20]);
        assert_eq!(ring.oldest(), Some(&20));
    }

    #[test]
    fn empty_buffer_has_no_ends() {
        let ring: RingBuffer<u8> = RingBuffer::with_capacity(8);
        assert!(ring.is_empty());
        assert_eq!(ring.last(), None);
        assert_eq!(ring.oldest(), None);
    }
}
