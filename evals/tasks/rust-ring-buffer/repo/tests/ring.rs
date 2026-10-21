use ring::RingBuffer;

#[test]
fn keeps_items_in_order() {
    let mut ring = RingBuffer::new(3);
    ring.push(1);
    ring.push(2);
    assert_eq!(ring.to_vec(), vec![1, 2]);
    assert_eq!(ring.pop(), Some(1));
    assert_eq!(ring.len(), 1);
}

#[test]
fn overwrites_the_oldest_when_full() {
    let mut ring = RingBuffer::new(3);
    for n in 1..=7 {
        ring.push(n);
    }
    assert_eq!(ring.len(), 3);
    assert_eq!(ring.to_vec(), vec![5, 6, 7]);
    assert_eq!(ring.pop(), Some(5));
    assert_eq!(ring.pop(), Some(6));
    assert_eq!(ring.pop(), Some(7));
    assert_eq!(ring.pop(), None);
    assert!(ring.is_empty());
}

#[test]
fn keeps_working_after_many_wraps() {
    let mut ring = RingBuffer::new(4);
    for n in 0..1000 {
        ring.push(n);
        if n % 3 == 0 {
            ring.pop();
        }
    }
    assert_eq!(ring.to_vec(), vec![997, 998, 999]);
}
