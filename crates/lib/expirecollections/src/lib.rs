use std::{
	collections::{HashMap, HashSet},
	hash::Hash,
	ops::Add,
	time::{Duration, SystemTime},
};

#[derive(Hash, PartialOrd, Ord, PartialEq, Eq)]
struct ValueContainer<V> {
	value: V,
	expire_time: SystemTime,
}

impl<V> ValueContainer<V> {
	fn new(value: V, expire_time: SystemTime) -> Self {
		ValueContainer { value, expire_time }
	}

	fn expire(&self) -> bool {
		SystemTime::now().le(&self.expire_time)
	}
}

pub struct ExpiringSet<V> {
	inner: HashSet<ValueContainer<V>>,
	time_to_live: Duration,
}

impl<V> ExpiringSet<V>
where
	V: PartialEq + Eq + Hash,
{
	pub fn new(time_to_live: Duration) -> Self {
		ExpiringSet {
			inner: HashSet::new(),
			time_to_live,
		}
	}

	pub fn insert(&mut self, v: V) {
		let value_container = ValueContainer::new(v, SystemTime::now().add(self.time_to_live));
		self.inner.insert(value_container);
	}

	pub fn contains(&mut self, v: V) -> bool {
		self.inner
			.iter()
			.filter(|&x| x.value == v)
			.next()
			.is_some_and(|x| SystemTime::now().le(&x.expire_time))
	}

	pub fn remove_expired_entries(&mut self) {
		self.inner.retain(|v| v.expire());
	}
}

pub struct ExpiringMap<K, V> {
	inner: HashMap<K, ValueContainer<V>>,
	time_to_live: Duration,
}

impl<K, V> ExpiringMap<K, V>
where
	K: Hash + Eq,
{
	pub fn new(time_to_live: Duration) -> Self {
		ExpiringMap {
			inner: HashMap::new(),
			time_to_live: time_to_live,
		}
	}

	pub fn insert(&mut self, k: K, v: V) -> Option<V> {
		let value_container = ValueContainer::new(v, SystemTime::now().add(self.time_to_live));

		self.inner.insert(k, value_container).and_then(|x| {
			if SystemTime::now().le(&x.expire_time) {
				Some(x.value)
			} else {
				None
			}
		})
	}

	pub fn contains(&mut self, k: &K) -> bool {
		self.inner.contains_key(k)
	}

	pub fn get(&mut self, k: &K) -> Option<&V> {
		self.inner.get(k).and_then(|vc| {
			if SystemTime::now().le(&vc.expire_time) {
				Some(&vc.value)
			} else {
				None
			}
		})
	}

	pub fn remove_expired_entries(&mut self) {
		self.inner.retain(|_, v| v.expire());
	}
}
