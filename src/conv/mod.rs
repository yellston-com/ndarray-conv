use std::fmt::Debug;

use ndarray::{
    Array, ArrayBase, ArrayView, Data, Dim, Dimension, IntoDimension, Ix, RemoveAxis, SliceArg,
    SliceInfo, SliceInfoElem,
};
use num::traits::NumAssign;

use crate::{
    dilation::{IntoKernelWithDilation, KernelWithDilation},
    padding::PaddingExt,
    ConvMode, PaddingMode,
};

pub struct ExplicitConv<const N: usize> {
    pub padding: [[usize; 2]; N],
    pub strides: [usize; N],
}

impl<const N: usize> ConvMode<N> {
    pub(crate) fn unfold<S>(self, kernel: &KernelWithDilation<S, N>) -> ExplicitConv<N>
    where
        S: ndarray::RawData,
        Dim<[Ix; N]>: Dimension,
        // [Ix; N]: IntoDimension<Dim = Dim<[Ix; N]>>,
    {
        let kernel_dim = kernel.kernel.raw_dim();
        let kernel_dim: [usize; N] = std::array::from_fn(|i| 
                // k + (k - 1) * (d - 1)
                kernel_dim[i] * kernel.dilation[i] - kernel.dilation[i] + 1);

        match self {
            ConvMode::Full => ExplicitConv {
                padding: std::array::from_fn(|i| [kernel_dim[i] - 1; 2]),
                strides: [1; N],
            },
            ConvMode::Same => ExplicitConv {
                padding: std::array::from_fn(|i| {
                    let k_size = kernel_dim[i];
                    if k_size % 2 == 0 {
                        [(k_size - 1) / 2 + 1, (k_size - 1) / 2]
                    } else {
                        [(k_size - 1) / 2; 2]
                    }
                }),
                strides: [1; N],
            },
            ConvMode::Valid => ExplicitConv {
                padding: [[0; 2]; N],
                strides: [1; N],
            },
            ConvMode::Custom { padding, strides } => ExplicitConv {
                padding: padding.map(|pad| [pad; 2]),
                strides,
            },
            ConvMode::Explicit { padding, strides } => ExplicitConv { padding, strides },
        }
    }
}

pub trait ConvExt<'a, T: NumAssign + Copy, S: ndarray::RawData, const N: usize> {
    fn conv(
        &self,
        kernel: impl IntoKernelWithDilation<'a, S, N>,
        conv_mode: ConvMode<N>,
        padding_mode: PaddingMode<N, T>,
    ) -> Option<Array<T, Dim<[Ix; N]>>>;
}

impl<'a, T: NumAssign + Copy, S: ndarray::RawData, const N: usize> ConvExt<'a, T, S, N>
    for ArrayBase<S, Dim<[Ix; N]>>
where
    T: num::traits::NumAssign + Copy + Debug,
    S: Data<Elem = T> + 'a,
    Dim<[Ix; N]>: Dimension,
    [Ix; N]: IntoDimension<Dim = Dim<[Ix; N]>>,
    SliceInfo<[SliceInfoElem; N], Dim<[Ix; N]>, Dim<[Ix; N]>>: SliceArg<Dim<[Ix; N]>>,
    Dim<[Ix; N]>: RemoveAxis,
{
    fn conv(
        &self,
        kernel: impl IntoKernelWithDilation<'a, S, N>,
        conv_mode: ConvMode<N>,
        padding_mode: PaddingMode<N, T>,
    ) -> Option<Array<T, Dim<[Ix; N]>>> {
        let kernel = kernel.into_kernel_with_dilation();

        let cm = conv_mode.unfold(&kernel);
        let pds = self.padding(padding_mode, cm.padding);

        let offset_list = kernel.gen_offset_list(pds.strides());

        let self_raw_dim = self.raw_dim();
        let kernel_raw_dim = kernel.kernel.raw_dim();
        let kernel_raw_dim_with_dilation: [usize; N] = std::array::from_fn(|i| {
            kernel_raw_dim[i] * kernel.dilation[i] - kernel.dilation[i] + 1
        });
        let output_shape: [usize; N] = std::array::from_fn(|i| {
            (cm.padding[i][0] + cm.padding[i][1] + self_raw_dim[i]
                - kernel_raw_dim_with_dilation[i])
                / cm.strides[i]
                + 1
        });
        let mut ret = Array::zeros(output_shape);

        let shape: [usize; N] = std::array::from_fn(|i| ret.raw_dim()[i]);
        let strides: [usize; N] =
            std::array::from_fn(|i| cm.strides[i] * pds.strides()[i] as usize);

        // dbg!(&offset_list);
        // dbg!(strides);

        unsafe {
            // use raw pointer to improve performance.
            let p: *mut T = ret.as_mut_ptr();

            // use ArrayView's iter without handle strides
            let view = ArrayView::from_shape(
                ndarray::ShapeBuilder::strides(shape, strides),
                pds.as_slice().unwrap(),
            )
            .unwrap();

            view.iter().enumerate().for_each(|(i, cur)| {
                let mut tmp_res = T::zero();

                offset_list.iter().for_each(|(tmp_offset, tmp_kernel)| {
                    tmp_res += *(cur as *const T).offset(*tmp_offset) * *tmp_kernel
                });

                *p.add(i) = tmp_res;
            });
        }

        Some(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dilation::WithDilation;
    use ndarray::prelude::*;

    #[test]
    fn tch_conv2d() {
        // let a = vec![1, 2, 3];
        // let b = a
        //     .iter()
        //     .flat_map(|&v| std::iter::repeat(v).take(4))
        //     .collect::<Vec<_>>();
        let tensor = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 3, 3]);
        let kernel = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 2, 3]);
        // let kernel = tch::Tensor::from_slice2(&[[1]])
        //     .to_dtype(tch::Kind::Float, false, true)
        //     .reshape([1, 1, 1, 1]);
        // let result = tensor.f_conv2d::<tch::Tensor>(&kernel, None, 1, [1, 1], 3i64, 1);
        let result = tensor.f_conv2d_padding::<tch::Tensor>(&kernel, None, 1, "same", 2, 1);
        result.unwrap().print();

        let arr = array![[1, 1, 1], [1, 1, 1], [1, 1, 1]];
        let kernel = array![[1, 1, 1], [1, 1, 1]];

        let res = arr
            .conv(kernel.with_dilation(2), ConvMode::Same, PaddingMode::Zeros)
            .unwrap();
        dbg!(res);
    }

    #[test]
    fn test_conv() {
        let arr = array![[1, 2], [3, 4]];
        let kernel = array![[1, 1], [1, 1]];

        let res = arr
            .conv(
                &kernel,
                ConvMode::Custom {
                    padding: [1, 2],
                    strides: [2, 2],
                },
                PaddingMode::Zeros,
            )
            .unwrap();
        assert_eq!(res, array![[0, 3, 0], [0, 7, 0]]);
        dbg!(res);

        let arr = array![[1, 2], [3, 4]];
        let kernel = array![[1, 1], [1, 1]];

        let res = arr
            .conv(&kernel, ConvMode::Full, PaddingMode::Zeros)
            .unwrap();
        assert_eq!(res, array![[1, 3, 2], [4, 10, 6], [3, 7, 4]]);
        dbg!(res);

        let arr = array![1, 2, 3, 4, 5, 6];
        let kernel = array![1, 1, 1];

        let res = arr
            .conv(
                &kernel,
                ConvMode::Custom {
                    padding: [4],
                    strides: [2],
                },
                PaddingMode::Zeros,
            )
            .unwrap();
        assert_eq!(res, array![0, 1, 6, 12, 11, 0]);
        dbg!(res);

        let arr = array![1, 2, 3, 4, 5, 6];
        let kernel = array![1, 1, 1];

        let res = arr
            .conv(
                kernel.with_dilation(2),
                ConvMode::Custom {
                    padding: [4],
                    strides: [2],
                },
                PaddingMode::Zeros,
            )
            .unwrap();
        assert_eq!(res, array![1, 4, 9, 8, 5]);
        dbg!(res);
    }

    #[test]
    fn aligned_with_libtorch() {
        let tensor = tch::Tensor::from_slice(&[1, 2, 3, 4, 5, 6])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 6]);
        let kernel = tch::Tensor::from_slice(&[1, 1, 1])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 3]);

        let result = tensor.f_conv1d::<tch::Tensor>(&kernel, None, 2, 4, 2, 1);
        result.unwrap().print();

        let arr = array![1, 2, 3, 4, 5, 6];
        let kernel = array![1, 1, 1];

        let res = arr
            .conv(
                kernel.with_dilation(2),
                ConvMode::Custom {
                    padding: [4],
                    strides: [2],
                },
                PaddingMode::Zeros,
            )
            .unwrap();
        assert_eq!(res, array![1, 4, 9, 8, 5]);
        dbg!(res);

        //

        let tensor = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 3, 3]);
        let kernel = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 3, 3]);

        let result = tensor.f_conv2d_padding::<tch::Tensor>(&kernel, None, 1, "same", 2, 1);
        result.unwrap().print();

        let arr = array![[1, 1, 1], [1, 1, 1], [1, 1, 1]];
        let kernel = array![[1, 1, 1], [1, 1, 1,], [1, 1, 1]];

        let res = arr
            .conv(kernel.with_dilation(2), ConvMode::Same, PaddingMode::Zeros)
            .unwrap();
        assert_eq!(res, array![[4, 2, 4], [2, 1, 2], [4, 2, 4]]);
        dbg!(res);

        //

        let tensor = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 3, 3]);
        let kernel = tch::Tensor::from_slice2(&[[1, 1, 1], [1, 1, 1]])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 2, 3]);

        let result = tensor.f_conv2d_padding::<tch::Tensor>(&kernel, None, 1, "same", 2, 1);
        result.unwrap().print();

        let arr = array![[1, 1, 1], [1, 1, 1], [1, 1, 1]];
        let kernel = array![[1, 1, 1], [1, 1, 1]];

        let res = arr
            .conv(kernel.with_dilation(2), ConvMode::Same, PaddingMode::Zeros)
            .unwrap();
        assert_eq!(res, array![[2, 1, 2], [4, 2, 4], [2, 1, 2]]);
        dbg!(res);

        //

        let tensor = tch::Tensor::from_slice(&[1, 2, 3, 4, 5, 6, 7, 8])
            .to_dtype(tch::Kind::Float, false, true)
            .reshape([1, 1, 2, 2, 2]);
        let kernel = tch::Tensor::from_slice(&[
            // 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ])
        .to_dtype(tch::Kind::Float, false, true)
        .reshape([1, 1, 2, 3, 3]);

        let result = tensor.f_conv3d::<tch::Tensor>(&kernel, None, [1, 2, 1], 2, 2, 1);
        result.unwrap().print();

        let arr = array![[[1, 2], [3, 4]], [[5, 6], [7, 8]]];
        let kernel = array![
            [[1, 1, 1], [1, 1, 1], [1, 1, 1]],
            [[1, 1, 1], [1, 1, 1], [1, 1, 1]],
        ];

        let res = arr
            .conv(
                kernel.with_dilation(2),
                ConvMode::Custom {
                    padding: [2, 2, 2],
                    strides: [1, 2, 1],
                },
                PaddingMode::Zeros,
            )
            .unwrap();
        assert_eq!(res, array![[[1, 2]], [[5, 6]], [[1, 2]], [[5, 6]]]);
        dbg!(res);
    }
}
