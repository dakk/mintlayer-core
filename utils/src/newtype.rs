#[macro_export]
macro_rules! newtype {
    ($(#[$meta:meta])* $vis:vis struct $name:ident($wrapped:ty)) => {
        $(#[$meta])*
        $vis struct $name($wrapped);

        impl From<$name> for $wrapped {
            fn from(newtype_instance: $name) -> Self {
                newtype_instance.0
            }
        }

        impl From<$wrapped> for $name {
            fn from(inner: $wrapped) -> Self {
                Self(inner)
            }
        }

        impl std::ops::Deref for $name {
            type Target = $wrapped;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }
    };
}

#[cfg(test)]
mod tests {
    #[derive(Clone, Debug)]
    struct OldInt {
        val: u32,
    }

    impl OldInt {
        fn new() -> Self {
            Self { val: 0 }
        }
        fn set(&mut self, val: u32) {
            self.val = val
        }
        fn get(&self) -> u32 {
            self.val
        }
    }

    newtype!(
        #[derive(Clone, Debug)]
        struct NewInt(OldInt));

    #[test]
    fn test_new_type() {
        let old = OldInt::new();
        let mut new = NewInt::from(old);
        let val = 7;
        new.set(val);
        assert_eq!(new.get(), val);
        let old_again = OldInt::from(new);
        assert_eq!(old_again.get(), val);
    }
}
