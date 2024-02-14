// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use crate::handles::{HandlesTransport, TransferHandles};
use crate::platform::Message;

impl<Item> Message<Item> {
    pub fn ref_item(&self) -> &Item {
        &self.item
    }
}

impl<T> TransferHandles for Message<T>
where
    T: TransferHandles,
{
    fn move_handles<M>(&self, mover: M) -> Result<(), M::Error>
    where
        M: HandlesTransport,
    {
        self.item.move_handles(mover)
    }

    fn receive_handles<P>(&mut self, provider: P) -> Result<(), P::Error>
    where
        P: HandlesTransport,
    {
        self.item.receive_handles(provider)
    }
}
