/// A partial reimpl of Vec with a larger struct (T)
/// and an extracted field (S); where iterating
/// over S is much faster than over T due to
/// cache coherence and/or primative SIMD optimization
pub(crate) struct SpeedVec<'a, T, S>
where
  S: Copy,
{
  full_item: Vec<T>,
  contiguous_access: Vec<S>,
  extractor: &'a (dyn Fn(&T) -> S + Send + Sync),
}

impl<'a, T, S> SpeedVec<'a, T, S>
where
  S: Copy,
{
  #[inline]
  pub(crate) fn with_capacity(
    capacity: usize,
    extractor: &'a (dyn Fn(&T) -> S + Send + Sync),
  ) -> Self {
    Self {
      full_item: Vec::with_capacity(capacity),
      contiguous_access: Vec::with_capacity(capacity),
      extractor,
    }
  }
  #[inline]
  pub(crate) fn push(&mut self, item: T) {
    self.contiguous_access.push((self.extractor)(&item));
    self.full_item.push(item);
  }
  #[inline]
  pub(crate) fn clear(&mut self) {
    self.contiguous_access.clear();
    self.full_item.clear();
  }
  #[inline]
  pub(crate) fn swap_remove(&mut self, index: usize) -> T {
    self.contiguous_access.swap_remove(index);
    self.full_item.swap_remove(index)
  }
  #[inline]
  pub(crate) fn iter_fast(&self) -> std::slice::Iter<S> {
    self.contiguous_access.iter()
  }
  #[inline]
  pub(crate) fn get(&self, index: usize) -> Option<&T> {
    self.full_item.get(index)
  }
  #[inline]
  pub(crate) fn get_mut(&mut self, index: usize) -> ModifyGuard<T, S> {
    ModifyGuard {
      inner: self.full_item.get_mut(index),
      derived: self.contiguous_access.get_mut(index),
      extractor: &self.extractor,
    }
  }
  #[inline]
  pub(crate) fn len(&self) -> usize {
    self.contiguous_access.len()
  }
  #[inline]
  pub(crate) fn inner(&self) -> &[T] {
    &self.full_item
  }
}

pub(crate) struct ModifyGuard<'a, T, S>
where
  S: Copy,
{
  inner: Option<&'a mut T>,
  derived: Option<&'a mut S>,
  extractor: &'a dyn Fn(&T) -> S,
}

impl<'a, T, S> Drop for ModifyGuard<'a, T, S>
where
  S: Copy,
{
  #[inline]
  fn drop(&mut self) {
    // Borrow checking requires we extract these references from the owning object, or we can't compile
    let inner = self.inner.as_mut();
    let mut derived = self.derived.as_mut();
    let extractor = self.extractor;
    if let Some(item) = derived.as_mut() {
      ***item = extractor(inner.as_ref().unwrap());
    }
  }
}

impl<'a, T, S> std::ops::Deref for ModifyGuard<'a, T, S>
where
  S: Copy,
{
  type Target = Option<&'a mut T>;
  #[inline]
  fn deref(&self) -> &Self::Target {
    &self.inner
  }
}
impl<'a, T, S> std::ops::DerefMut for ModifyGuard<'a, T, S>
where
  S: Copy,
{
  #[inline]
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.inner
  }
}

mod test {
  #[test]
  fn modification_consistency() {
    use super::*;
    let mut items = SpeedVec::with_capacity(10, &|item| item * item);
    items.push(1);
    items.push(2);
    items.push(3);
    items.push(4);
    {
      let mut fiter = items.iter_fast();
      assert_eq!(fiter.next(), Some(&1));
      assert_eq!(fiter.next(), Some(&4));
      assert_eq!(fiter.next(), Some(&9));
      assert_eq!(fiter.next(), Some(&16));
    }
    if let Some(item) = items.get_mut(2).as_mut() {
      **item = 5;
    }
    let mut fiter = items.iter_fast();
    assert_eq!(fiter.next(), Some(&1));
    assert_eq!(fiter.next(), Some(&4));
    assert_eq!(fiter.next(), Some(&25));
    assert_eq!(fiter.next(), Some(&16));
  }
}
