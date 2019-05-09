/// Similar to the `TryFrom` trait proposed but not yet available in `std`.
pub trait TryFrom<T>: Sized {
    type Error;

    fn try_from(t: T) -> Result<Self, Self::Error>;
}

/// Similar to the `TryInto` trait proposed but not yet available in
/// `std`. Blanket implemented for all `TryFrom`.
pub trait TryInto<T>: Sized {
    type Error;

    fn try_into(self) -> Result<T, Self::Error>;
}

impl<T, F> TryInto<F> for T
    where F: TryFrom<T>
{
    type Error = F::Error;

    fn try_into(self) -> Result<F, Self::Error> {
        F::try_from(self)
    }
}
