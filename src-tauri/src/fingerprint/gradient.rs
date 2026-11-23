use vstd::prelude::*;

verus! {

spec fn bit_set(hash: u64, index: usize) -> bool {
    (hash >> index) & 1u64 == 1
}

// Each row contributes eight comparisons between its nine pixels.
spec fn darkens(pixels: Seq<u8>, index: usize) -> bool {
    let left = ((index as int) / 8) * 9 + (index as int) % 8;
    pixels[left] > pixels[left + 1]
}

proof fn set_bit(hash: u64, bit: usize, index: usize)
    requires bit < 64, index < 64,
    ensures bit_set(hash | (1u64 << bit), index) == (bit == index || bit_set(hash, index)),
{
    assert((((hash | (1u64 << bit)) >> index) & 1u64 == 1)
        == (bit == index || ((hash >> index) & 1u64 == 1))) by (bit_vector)
        requires bit < 64, index < 64;
}

pub(crate) fn frame_gradient(pixels: &[u8; 72]) -> (hash: u64)
    ensures forall|index: usize| index < 64 ==> #[trigger] bit_set(hash, index) == darkens(pixels@, index),
{
    let mut hash = 0u64;
    let mut bit = 0usize;
    proof {
        assert forall|index: usize| index < 64 implies !#[trigger] bit_set(0, index) by {
            assert((0u64 >> index) & 1u64 == 0) by (bit_vector);
        }
    }
    while bit < 64
        invariant
            bit <= 64,
            forall|index: usize| index < 64 ==> #[trigger] bit_set(hash, index)
                == (index < bit && darkens(pixels@, index)),
        decreases 64 - bit,
    {
        let left = (bit / 8) * 9 + bit % 8;
        let darker = pixels[left] > pixels[left + 1];
        let ghost before = hash;
        if darker {
            hash |= 1u64 << bit;
        }
        proof {
            assert forall|index: usize| index < 64 implies #[trigger] bit_set(hash, index)
                == (index <= bit && darkens(pixels@, index)) by {
                if darker {
                    set_bit(before, bit, index);
                }
            }
        }
        bit += 1;
    }
    hash
}

} // verus!
