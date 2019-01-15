/// Similar to the `TryFrom` trait proposed but not yet available in `std`.
pub trait TryFrom<T>: Sized {
    type Err: 'static;

    fn try_from(t: T) -> Result<Self, Self::Err>;
}

/// Similar to the `TryInto` trait proposed but not yet available in
/// `std`. Blanket implemented for all `TryFrom`.
pub trait TryInto<T>: Sized {
    type Err: 'static;

    fn try_into(self) -> Result<T, Self::Err>;
}

impl<T, F> TryInto<F> for T
    where F: TryFrom<T>
{
    type Err = F::Err;

    fn try_into(self) -> Result<F, Self::Err> {
        F::try_from(self)
    }
}
