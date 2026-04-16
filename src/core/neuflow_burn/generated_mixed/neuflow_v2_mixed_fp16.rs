// Generated from ONNX "C:/Users/Jhe/Desktop/github/gyroflow-burn-neuflow/resources/neuflow_v2_mixed_fp16.onnx" by burn-onnx
use burn::prelude::*;
use burn::nn::BatchNorm;
use burn::nn::BatchNormConfig;
use burn::nn::LayerNorm;
use burn::nn::LayerNormConfig;
use burn::nn::Linear;
use burn::nn::LinearConfig;
use burn::nn::PaddingConfig2d;
use burn::nn::conv::Conv2d;
use burn::nn::conv::Conv2dConfig;
use burn::nn::pool::AvgPool2d;
use burn::nn::pool::AvgPool2dConfig;
use burn::tensor::Bytes;
use burn_store::BurnpackStore;
use burn_store::ModuleSnapshot;


#[derive(Module, Debug)]
pub struct Submodule1<B: Backend> {
    constant75: burn::module::Param<Tensor<B, 1>>,
    averagepool2d1: AvgPool2d,
    conv2d1: Conv2d<B>,
    conv2d2: Conv2d<B>,
    averagepool2d2: AvgPool2d,
    conv2d3: Conv2d<B>,
    conv2d4: Conv2d<B>,
    conv2d5: Conv2d<B>,
    conv2d6: Conv2d<B>,
    averagepool2d3: AvgPool2d,
    conv2d7: Conv2d<B>,
    conv2d8: Conv2d<B>,
    conv2d9: Conv2d<B>,
    conv2d10: Conv2d<B>,
    conv2d11: Conv2d<B>,
    conv2d12: Conv2d<B>,
    constant77: burn::module::Param<Tensor<B, 4>>,
    linear1: Linear<B>,
    linear2: Linear<B>,
    linear3: Linear<B>,
    constant109: burn::module::Param<Tensor<B, 1>>,
    linear4: Linear<B>,
    layernormalization1: LayerNorm<B>,
    linear5: Linear<B>,
    constant81: burn::module::Param<Tensor<B, 1>>,
    constant82: burn::module::Param<Tensor<B, 1>>,
    constant83: burn::module::Param<Tensor<B, 1>>,
    linear6: Linear<B>,
    layernormalization2: LayerNorm<B>,
    linear7: Linear<B>,
    linear8: Linear<B>,
    linear9: Linear<B>,
    linear10: Linear<B>,
    layernormalization3: LayerNorm<B>,
    linear11: Linear<B>,
    linear12: Linear<B>,
    layernormalization4: LayerNorm<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule1<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let constant75: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([255f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let averagepool2d1 = AvgPool2dConfig::new([2, 2])
            .with_strides([2, 2])
            .with_padding(PaddingConfig2d::Valid)
            .with_count_include_pad(true)
            .with_ceil_mode(false)
            .init();
        let conv2d1 = Conv2dConfig::new([3, 256], [8, 8])
            .with_stride([4, 4])
            .with_padding(PaddingConfig2d::Explicit(2, 2, 2, 2))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d2 = Conv2dConfig::new([256, 256], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let averagepool2d2 = AvgPool2dConfig::new([2, 2])
            .with_strides([2, 2])
            .with_padding(PaddingConfig2d::Valid)
            .with_count_include_pad(true)
            .with_ceil_mode(false)
            .init();
        let conv2d3 = Conv2dConfig::new([3, 128], [6, 6])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(2, 2, 2, 2))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d4 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d5 = Conv2dConfig::new([384, 192], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d6 = Conv2dConfig::new([192, 192], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let averagepool2d3 = AvgPool2dConfig::new([2, 2])
            .with_strides([2, 2])
            .with_padding(PaddingConfig2d::Valid)
            .with_count_include_pad(true)
            .with_ceil_mode(false)
            .init();
        let conv2d7 = Conv2dConfig::new([3, 128], [6, 6])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(2, 2, 2, 2))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d8 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d9 = Conv2dConfig::new([192, 128], [6, 6])
            .with_stride([2, 2])
            .with_padding(PaddingConfig2d::Explicit(2, 2, 2, 2))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d10 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d11 = Conv2dConfig::new([256, 190], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d12 = Conv2dConfig::new([190, 190], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant77: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([2, 2, 27, 48], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [2, 2, 27, 48].into(),
        );
        let linear1 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let linear2 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let linear3 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let constant109: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([1], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1].into(),
        );
        let linear4 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let layernormalization1 = LayerNormConfig::new(192)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear5 = LinearConfig::new(384, 384).with_bias(false).init(device);
        let constant81: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1.4140625f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant82: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([1f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant83: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([0.5f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let linear6 = LinearConfig::new(384, 192).with_bias(false).init(device);
        let layernormalization2 = LayerNormConfig::new(192)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear7 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let linear8 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let linear9 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let linear10 = LinearConfig::new(192, 192).with_bias(true).init(device);
        let layernormalization3 = LayerNormConfig::new(192)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        let linear11 = LinearConfig::new(384, 384).with_bias(false).init(device);
        let linear12 = LinearConfig::new(384, 192).with_bias(false).init(device);
        let layernormalization4 = LayerNormConfig::new(192)
            .with_epsilon(0.000009999999747378752f64)
            .with_bias(true)
            .init(device);
        Self {
            constant75,
            averagepool2d1,
            conv2d1,
            conv2d2,
            averagepool2d2,
            conv2d3,
            conv2d4,
            conv2d5,
            conv2d6,
            averagepool2d3,
            conv2d7,
            conv2d8,
            conv2d9,
            conv2d10,
            conv2d11,
            conv2d12,
            constant77,
            linear1,
            linear2,
            linear3,
            constant109,
            linear4,
            layernormalization1,
            linear5,
            constant81,
            constant82,
            constant83,
            linear6,
            layernormalization2,
            linear7,
            linear8,
            linear9,
            linear10,
            layernormalization3,
            linear11,
            linear12,
            layernormalization4,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        img0: Tensor<B, 4>,
        img1: Tensor<B, 4>,
    ) -> (
        Tensor<B, 3>,
        Tensor<B, 4>,
        Tensor<B, 1>,
        Tensor<B, 1>,
        Tensor<B, 1>,
        Tensor<B, 4>,
    ) {
        let constant75_out1 = self.constant75.val();
        let div1_out1 = img0
            .div((constant75_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div2_out1 = img1
            .div((constant75_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat1_out1 = burn::tensor::Tensor::cat(
            [div1_out1.clone(), div2_out1].into(),
            0,
        );
        let averagepool2d1_out1 = self.averagepool2d1.forward(concat1_out1);
        let conv2d1_out1 = self.conv2d1.forward(averagepool2d1_out1.clone());
        let leakyrelu1_out1 = burn::tensor::activation::leaky_relu(
            conv2d1_out1,
            0.10000000149011612,
        );
        let conv2d2_out1 = self.conv2d2.forward(leakyrelu1_out1);
        let leakyrelu2_out1 = burn::tensor::activation::leaky_relu(
            conv2d2_out1,
            0.10000000149011612,
        );
        let averagepool2d2_out1 = self.averagepool2d2.forward(averagepool2d1_out1);
        let conv2d3_out1 = self.conv2d3.forward(averagepool2d2_out1.clone());
        let leakyrelu3_out1 = burn::tensor::activation::leaky_relu(
            conv2d3_out1,
            0.10000000149011612,
        );
        let conv2d4_out1 = self.conv2d4.forward(leakyrelu3_out1);
        let leakyrelu4_out1 = burn::tensor::activation::leaky_relu(
            conv2d4_out1,
            0.10000000149011612,
        );
        let concat2_out1 = burn::tensor::Tensor::cat(
            [leakyrelu2_out1, leakyrelu4_out1].into(),
            1,
        );
        let conv2d5_out1 = self.conv2d5.forward(concat2_out1);
        let leakyrelu5_out1 = burn::tensor::activation::leaky_relu(
            conv2d5_out1,
            0.10000000149011612,
        );
        let conv2d6_out1 = self.conv2d6.forward(leakyrelu5_out1);
        let leakyrelu6_out1 = burn::tensor::activation::leaky_relu(
            conv2d6_out1,
            0.10000000149011612,
        );
        let averagepool2d3_out1 = self.averagepool2d3.forward(averagepool2d2_out1);
        let conv2d7_out1 = self.conv2d7.forward(averagepool2d3_out1);
        let leakyrelu7_out1 = burn::tensor::activation::leaky_relu(
            conv2d7_out1,
            0.10000000149011612,
        );
        let conv2d8_out1 = self.conv2d8.forward(leakyrelu7_out1);
        let leakyrelu8_out1 = burn::tensor::activation::leaky_relu(
            conv2d8_out1,
            0.10000000149011612,
        );
        let conv2d9_out1 = self.conv2d9.forward(leakyrelu6_out1.clone());
        let leakyrelu9_out1 = burn::tensor::activation::leaky_relu(
            conv2d9_out1,
            0.10000000149011612,
        );
        let conv2d10_out1 = self.conv2d10.forward(leakyrelu9_out1);
        let leakyrelu10_out1 = burn::tensor::activation::leaky_relu(
            conv2d10_out1,
            0.10000000149011612,
        );
        let concat3_out1 = burn::tensor::Tensor::cat(
            [leakyrelu8_out1, leakyrelu10_out1].into(),
            1,
        );
        let conv2d11_out1 = self.conv2d11.forward(concat3_out1);
        let leakyrelu11_out1 = burn::tensor::activation::leaky_relu(
            conv2d11_out1,
            0.10000000149011612,
        );
        let conv2d12_out1 = self.conv2d12.forward(leakyrelu11_out1);
        let leakyrelu12_out1 = burn::tensor::activation::leaky_relu(
            conv2d12_out1,
            0.10000000149011612,
        );
        let constant77_out1 = self.constant77.val();
        let concat4_out1 = burn::tensor::Tensor::cat(
            [leakyrelu12_out1, constant77_out1].into(),
            1,
        );
        let reshape1_out1 = concat4_out1.reshape([2, 192, -1]);
        let transpose1_out1 = reshape1_out1.permute([0, 2, 1]);
        let slice1_out1 = transpose1_out1.clone().slice(s![0..1, .., ..]);
        let slice2_out1 = transpose1_out1.clone().slice(s![1..2, .., ..]);
        let concat5_out1 = burn::tensor::Tensor::cat(
            [slice2_out1, slice1_out1].into(),
            0,
        );
        let linear1_out1 = self.linear1.forward(transpose1_out1.clone());
        let linear2_out1 = self.linear2.forward(concat5_out1.clone());
        let linear3_out1 = self.linear3.forward(concat5_out1);
        let transpose2_out1 = linear2_out1.permute([0, 2, 1]);
        let constant109_out1 = self.constant109.val();
        let mul1_out1 = linear1_out1
            .mul((constant109_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul2_out1 = transpose2_out1
            .mul((constant109_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let matmul4_out1 = mul1_out1.matmul(mul2_out1);
        let softmax1_out1 = {
            let dtype = matmul4_out1.dtype();
            burn::tensor::activation::softmax(matmul4_out1.cast(burn::tensor::DType::F32), 2)
                .cast(dtype)
        };
        let matmul5_out1 = softmax1_out1.matmul(linear3_out1);
        let linear4_out1 = self.linear4.forward(matmul5_out1);
        let layernormalization1_out1 = {
            let dtype = linear4_out1.dtype();
            self.layernormalization1
                .forward(linear4_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let concat6_out1 = burn::tensor::Tensor::cat(
            [transpose1_out1.clone(), layernormalization1_out1].into(),
            2,
        );
        let linear5_out1 = self.linear5.forward(concat6_out1);
        let constant81_out1 = self.constant81.val();
        let div3_out1 = linear5_out1
            .clone()
            .div((constant81_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf1_out1 = div3_out1.erf();
        let constant82_out1 = self.constant82.val();
        let add1_out1 = erf1_out1
            .add((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul3_out1 = linear5_out1.mul(add1_out1);
        let constant83_out1 = self.constant83.val();
        let mul4_out1 = mul3_out1
            .mul((constant83_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear6_out1 = self.linear6.forward(mul4_out1);
        let layernormalization2_out1 = {
            let dtype = linear6_out1.dtype();
            self.layernormalization2
                .forward(linear6_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add2_out1 = transpose1_out1.add(layernormalization2_out1);
        let slice3_out1 = add2_out1.clone().slice(s![0..1, .., ..]);
        let slice4_out1 = add2_out1.clone().slice(s![1..2, .., ..]);
        let concat7_out1 = burn::tensor::Tensor::cat(
            [slice4_out1, slice3_out1].into(),
            0,
        );
        let linear7_out1 = self.linear7.forward(add2_out1.clone());
        let linear8_out1 = self.linear8.forward(concat7_out1.clone());
        let linear9_out1 = self.linear9.forward(concat7_out1);
        let transpose3_out1 = linear8_out1.permute([0, 2, 1]);
        let mul5_out1 = linear7_out1
            .mul((constant109_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul6_out1 = transpose3_out1
            .mul((constant109_out1).unsqueeze_dims(&[0isize, 1isize]));
        let matmul12_out1 = mul5_out1.matmul(mul6_out1);
        let softmax2_out1 = {
            let dtype = matmul12_out1.dtype();
            burn::tensor::activation::softmax(matmul12_out1.cast(burn::tensor::DType::F32), 2)
                .cast(dtype)
        };
        let matmul13_out1 = softmax2_out1.matmul(linear9_out1);
        let linear10_out1 = self.linear10.forward(matmul13_out1);
        let layernormalization3_out1 = {
            let dtype = linear10_out1.dtype();
            self.layernormalization3
                .forward(linear10_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let concat8_out1 = burn::tensor::Tensor::cat(
            [add2_out1.clone(), layernormalization3_out1].into(),
            2,
        );
        let linear11_out1 = self.linear11.forward(concat8_out1);
        let div4_out1 = linear11_out1
            .clone()
            .div((constant81_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let erf2_out1 = div4_out1.erf();
        let add3_out1 = erf2_out1
            .add((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul7_out1 = linear11_out1.mul(add3_out1);
        let mul8_out1 = mul7_out1
            .mul((constant83_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let linear12_out1 = self.linear12.forward(mul8_out1);
        let layernormalization4_out1 = {
            let dtype = linear12_out1.dtype();
            self.layernormalization4
                .forward(linear12_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let add4_out1 = add2_out1.add(layernormalization4_out1);
        (
            add4_out1,
            leakyrelu6_out1,
            constant82_out1,
            constant81_out1,
            constant83_out1,
            div1_out1,
        )
    }
}
#[derive(Module, Debug)]
pub struct Submodule2<B: Backend> {
    batchnormalization1: BatchNorm<B>,
    constant140: burn::module::Param<Tensor<B, 1>>,
    constant86: burn::module::Param<Tensor<B, 3>>,
    constant87: burn::module::Param<Tensor<B, 4>>,
    constant88: burn::module::Param<Tensor<B, 1>>,
    constant89: burn::module::Param<Tensor<B, 4>>,
    constant90: burn::module::Param<Tensor<B, 1>>,
    constant91: burn::module::Param<Tensor<B, 1>>,
    constant92: burn::module::Param<Tensor<B, 1>>,
    constant93: burn::module::Param<Tensor<B, 4>>,
    constant94: burn::module::Param<Tensor<B, 4>>,
    conv2d13: Conv2d<B>,
    conv2d14: Conv2d<B>,
    conv2d15: Conv2d<B>,
    conv2d16: Conv2d<B>,
    conv2d17: Conv2d<B>,
    conv2d18: Conv2d<B>,
    conv2d19: Conv2d<B>,
    conv2d20: Conv2d<B>,
    resize1: burn::nn::interpolate::Interpolate2d,
    resize2: burn::nn::interpolate::Interpolate2d,
    conv2d21: Conv2d<B>,
    conv2d22: Conv2d<B>,
    resize3: burn::nn::interpolate::Interpolate2d,
    conv2d23: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule2<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let batchnormalization1 = BatchNormConfig::new(192)
            .with_epsilon(0.000009999999747378752f64)
            .with_momentum(0.8999999761581421f64)
            .init(device);
        let constant140: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::zeros([1], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1].into(),
        );
        let constant86: burn::module::Param<Tensor<B, 3>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                3,
            >::zeros([1, 1296, 2], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 1296, 2].into(),
        );
        let constant87: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 2, 27, 48], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 2, 27, 48].into(),
        );
        let constant88: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([11.3125f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant89: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1296, 9, 9, 2], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1296, 9, 9, 2].into(),
        );
        let constant90: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([2f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant91: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([47f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant92: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([26f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant93: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 64, 27, 48], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 64, 27, 48].into(),
        );
        let constant94: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 1, 27, 48], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 1, 27, 48].into(),
        );
        let conv2d13 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d14 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d15 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d16 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d17 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d18 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d19 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d20 = Conv2dConfig::new([128, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let resize1 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(None)
            .with_scale_factor(Some([2.0, 2.0]))
            .with_mode(burn::nn::interpolate::InterpolateMode::Nearest)
            .with_align_corners(false)
            .init();
        let resize2 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(None)
            .with_scale_factor(Some([2.0, 2.0]))
            .with_mode(burn::nn::interpolate::InterpolateMode::Nearest)
            .with_align_corners(false)
            .init();
        let conv2d21 = Conv2dConfig::new([256, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d22 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let resize3 = burn::nn::interpolate::Interpolate2dConfig::new()
            .with_output_size(None)
            .with_scale_factor(Some([2.0, 2.0]))
            .with_mode(burn::nn::interpolate::InterpolateMode::Nearest)
            .with_align_corners(false)
            .init();
        let conv2d23 = Conv2dConfig::new([128, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        Self {
            batchnormalization1,
            constant140,
            constant86,
            constant87,
            constant88,
            constant89,
            constant90,
            constant91,
            constant92,
            constant93,
            constant94,
            conv2d13,
            conv2d14,
            conv2d15,
            conv2d16,
            conv2d17,
            conv2d18,
            conv2d19,
            conv2d20,
            resize1,
            resize2,
            conv2d21,
            conv2d22,
            resize3,
            conv2d23,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        add4_out1: Tensor<B, 3>,
        leakyrelu6_out1: Tensor<B, 4>,
        constant82_out1: Tensor<B, 1>,
        constant81_out1: Tensor<B, 1>,
        constant83_out1: Tensor<B, 1>,
    ) -> (Tensor<B, 4>, Tensor<B, 4>, Tensor<B, 1>, Tensor<B, 4>) {
        let reshape2_out1 = add4_out1.reshape([2, 27, 48, 192]);
        let transpose4_out1 = reshape2_out1.permute([0, 3, 1, 2]);
        let batchnormalization1_out1 = self.batchnormalization1.forward(transpose4_out1);
        let split_tensors = batchnormalization1_out1
            .split_with_sizes([64, 128].into(), 1);
        let [split1_out1, split1_out2] = split_tensors.try_into().unwrap();
        let slice5_out1 = split1_out1.slice(s![0..1, .., .., ..]);
        let relu1_out1 = burn::tensor::activation::relu(slice5_out1);
        let split_tensors = leakyrelu6_out1.split_with_sizes([64, 128].into(), 1);
        let [split2_out1, split2_out2] = split_tensors.try_into().unwrap();
        let slice6_out1 = split2_out1.slice(s![0..1, .., .., ..]);
        let relu2_out1 = burn::tensor::activation::relu(slice6_out1);
        let slice7_out1 = split1_out2.clone().slice(s![0..1, .., .., ..]);
        let slice8_out1 = split1_out2.clone().slice(s![1..2, .., .., ..]);
        let reshape3_out1 = slice7_out1.clone().reshape([1, 128, -1]);
        let transpose5_out1 = reshape3_out1.permute([0, 2, 1]);
        let reshape4_out1 = slice8_out1.clone().reshape([1, 128, -1]);
        let constant140_out1 = self.constant140.val();
        let mul9_out1 = transpose5_out1
            .mul((constant140_out1.clone()).unsqueeze_dims(&[0isize, 1isize]));
        let mul10_out1 = reshape4_out1
            .mul((constant140_out1).unsqueeze_dims(&[0isize, 1isize]));
        let matmul17_out1 = mul9_out1.matmul(mul10_out1);
        let softmax3_out1 = {
            let dtype = matmul17_out1.dtype();
            burn::tensor::activation::softmax(matmul17_out1.cast(burn::tensor::DType::F32), 2)
                .cast(dtype)
        };
        let constant86_out1 = self.constant86.val();
        let matmul18_out1 = softmax3_out1.matmul(constant86_out1);
        let reshape5_out1 = matmul18_out1.reshape([1, 27, 48, 2]);
        let transpose6_out1 = reshape5_out1.permute([0, 3, 1, 2]);
        let constant87_out1 = self.constant87.val();
        let sub1_out1 = transpose6_out1.sub(constant87_out1.clone());
        let reshape6_out1 = slice7_out1.reshape([1, 128, 1296]);
        let reshape7_out1 = slice8_out1.reshape([1, 128, 1296]);
        let transpose7_out1 = reshape6_out1.permute([0, 2, 1]);
        let matmul19_out1 = transpose7_out1.matmul(reshape7_out1);
        let reshape8_out1 = matmul19_out1.reshape([1296, 1, 27, 48]);
        let constant88_out1 = self.constant88.val();
        let div5_out1 = reshape8_out1
            .div((constant88_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let add5_out1 = constant87_out1.add(sub1_out1.clone());
        let transpose8_out1 = add5_out1.permute([0, 2, 3, 1]);
        let reshape9_out1 = transpose8_out1.reshape([1296, 1, 1, 2]);
        let constant89_out1 = self.constant89.val();
        let add6_out1 = reshape9_out1.add(constant89_out1);
        let split_tensors = add6_out1.split_with_sizes([1, 1].into(), 3);
        let [split3_out1, split3_out2] = split_tensors.try_into().unwrap();
        let constant90_out1 = self.constant90.val();
        let mul11_out1 = split3_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant91_out1 = self.constant91.val();
        let div6_out1 = mul11_out1
            .div((constant91_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub2_out1 = div6_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul12_out1 = split3_out2
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant92_out1 = self.constant92.val();
        let div7_out1 = mul12_out1
            .div((constant92_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub3_out1 = div7_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat9_out1 = burn::tensor::Tensor::cat([sub2_out1, sub3_out1].into(), 3);
        let gridsample1_out1 = {
            let dtype = div5_out1.dtype();
            div5_out1.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat9_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape10_out1 = gridsample1_out1.reshape([1, 27, 48, -1]);
        let transpose9_out1 = reshape10_out1.permute([0, 3, 1, 2]);
        let constant93_out1 = self.constant93.val();
        let constant94_out1 = self.constant94.val();
        let concat10_out1 = burn::tensor::Tensor::cat(
            [
                transpose9_out1,
                relu1_out1.clone(),
                constant93_out1,
                sub1_out1.clone(),
                constant94_out1,
            ]
                .into(),
            1,
        );
        let conv2d13_out1 = self.conv2d13.forward(concat10_out1);
        let leakyrelu13_out1 = burn::tensor::activation::leaky_relu(
            conv2d13_out1,
            0.10000000149011612,
        );
        let conv2d14_out1 = self.conv2d14.forward(leakyrelu13_out1);
        let leakyrelu14_out1 = burn::tensor::activation::leaky_relu(
            conv2d14_out1,
            0.10000000149011612,
        );
        let conv2d15_out1 = self.conv2d15.forward(leakyrelu14_out1);
        let leakyrelu15_out1 = burn::tensor::activation::leaky_relu(
            conv2d15_out1,
            0.10000000149011612,
        );
        let conv2d16_out1 = self.conv2d16.forward(leakyrelu15_out1);
        let leakyrelu16_out1 = burn::tensor::activation::leaky_relu(
            conv2d16_out1,
            0.10000000149011612,
        );
        let conv2d17_out1 = self.conv2d17.forward(leakyrelu16_out1);
        let leakyrelu17_out1 = burn::tensor::activation::leaky_relu(
            conv2d17_out1,
            0.10000000149011612,
        );
        let conv2d18_out1 = self.conv2d18.forward(leakyrelu17_out1);
        let leakyrelu18_out1 = burn::tensor::activation::leaky_relu(
            conv2d18_out1,
            0.10000000149011612,
        );
        let conv2d19_out1 = self.conv2d19.forward(leakyrelu18_out1);
        let leakyrelu19_out1 = burn::tensor::activation::leaky_relu(
            conv2d19_out1,
            0.10000000149011612,
        );
        let conv2d20_out1 = self.conv2d20.forward(leakyrelu19_out1);
        let slice9_out1 = conv2d20_out1.slice(s![.., 0..2, .., ..]);
        let add7_out1 = sub1_out1.add(slice9_out1);
        let resize1_out1 = {
            let dtype = add7_out1.dtype();
            self.resize1
                .forward(add7_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let mul13_out1 = resize1_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let resize2_out1 = {
            let dtype = split1_out2.dtype();
            self.resize2
                .forward(split1_out2.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let concat11_out1 = burn::tensor::Tensor::cat(
            [split2_out2, resize2_out1].into(),
            1,
        );
        let conv2d21_out1 = self.conv2d21.forward(concat11_out1);
        let div8_out1 = conv2d21_out1
            .clone()
            .div((constant81_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let erf3_out1 = div8_out1.erf();
        let add8_out1 = erf3_out1
            .add((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul14_out1 = conv2d21_out1.mul(add8_out1);
        let mul15_out1 = mul14_out1
            .mul((constant83_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let conv2d22_out1 = self.conv2d22.forward(mul15_out1);
        let slice10_out1 = conv2d22_out1.clone().slice(s![0..1, .., .., ..]);
        let slice11_out1 = conv2d22_out1.slice(s![1..2, .., .., ..]);
        let reshape11_out1 = slice10_out1.reshape([1, 128, 5184]);
        let reshape12_out1 = slice11_out1.reshape([1, 128, 5184]);
        let transpose10_out1 = reshape11_out1.permute([0, 2, 1]);
        let matmul20_out1 = transpose10_out1.matmul(reshape12_out1);
        let reshape13_out1 = matmul20_out1.reshape([5184, 1, 54, 96]);
        let div9_out1 = reshape13_out1
            .div((constant88_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let resize3_out1 = {
            let dtype = relu1_out1.dtype();
            self.resize3
                .forward(relu1_out1.cast(burn::tensor::DType::F32))
                .cast(dtype)
        };
        let concat12_out1 = burn::tensor::Tensor::cat(
            [relu2_out1, resize3_out1].into(),
            1,
        );
        let conv2d23_out1 = self.conv2d23.forward(concat12_out1);
        let div10_out1 = conv2d23_out1
            .clone()
            .div((constant81_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let erf4_out1 = div10_out1.erf();
        let add9_out1 = erf4_out1
            .add((constant82_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul16_out1 = conv2d23_out1.mul(add9_out1);
        let mul17_out1 = mul16_out1
            .mul((constant83_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        (mul17_out1, mul13_out1, constant90_out1, div9_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule3<B: Backend> {
    conv2d24: Conv2d<B>,
    constant95: burn::module::Param<Tensor<B, 4>>,
    constant96: burn::module::Param<Tensor<B, 4>>,
    constant97: burn::module::Param<Tensor<B, 1>>,
    constant98: burn::module::Param<Tensor<B, 1>>,
    constant99: burn::module::Param<Tensor<B, 4>>,
    constant100: burn::module::Param<Tensor<B, 4>>,
    conv2d25: Conv2d<B>,
    conv2d26: Conv2d<B>,
    conv2d27: Conv2d<B>,
    conv2d28: Conv2d<B>,
    conv2d29: Conv2d<B>,
    conv2d30: Conv2d<B>,
    conv2d31: Conv2d<B>,
    conv2d32: Conv2d<B>,
    conv2d33: Conv2d<B>,
    conv2d34: Conv2d<B>,
    conv2d35: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule3<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d24 = Conv2dConfig::new([64, 64], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant95: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 2, 54, 96], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 2, 54, 96].into(),
        );
        let constant96: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([5184, 9, 9, 2], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [5184, 9, 9, 2].into(),
        );
        let constant97: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([95f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant98: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([53f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        let constant99: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 64, 54, 96], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 64, 54, 96].into(),
        );
        let constant100: burn::module::Param<Tensor<B, 4>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                4,
            >::zeros([1, 1, 54, 96], (device, burn::tensor::DType::F16)),
            device.clone(),
            false,
            [1, 1, 54, 96].into(),
        );
        let conv2d25 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d26 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d27 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d28 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d29 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d30 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d31 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d32 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d33 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d34 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d35 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        Self {
            conv2d24,
            constant95,
            constant96,
            constant97,
            constant98,
            constant99,
            constant100,
            conv2d25,
            conv2d26,
            conv2d27,
            conv2d28,
            conv2d29,
            conv2d30,
            conv2d31,
            conv2d32,
            conv2d33,
            conv2d34,
            conv2d35,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        mul17_out1: Tensor<B, 4>,
        mul13_out1: Tensor<B, 4>,
        constant90_out1: Tensor<B, 1>,
        constant82_out1: Tensor<B, 1>,
        div9_out1: Tensor<B, 4>,
    ) -> (
        Tensor<B, 4>,
        Tensor<B, 4>,
        Tensor<B, 4>,
        Tensor<B, 4>,
        Tensor<B, 1>,
        Tensor<B, 1>,
        Tensor<B, 4>,
        Tensor<B, 4>,
    ) {
        let conv2d24_out1 = self.conv2d24.forward(mul17_out1);
        let constant95_out1 = self.constant95.val();
        let add10_out1 = constant95_out1.clone().add(mul13_out1.clone());
        let transpose11_out1 = add10_out1.permute([0, 2, 3, 1]);
        let reshape14_out1 = transpose11_out1.reshape([5184, 1, 1, 2]);
        let constant96_out1 = self.constant96.val();
        let add11_out1 = reshape14_out1.add(constant96_out1.clone());
        let split_tensors = add11_out1.split_with_sizes([1, 1].into(), 3);
        let [split4_out1, split4_out2] = split_tensors.try_into().unwrap();
        let mul18_out1 = split4_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant97_out1 = self.constant97.val();
        let div11_out1 = mul18_out1
            .div((constant97_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub4_out1 = div11_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul19_out1 = split4_out2
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let constant98_out1 = self.constant98.val();
        let div12_out1 = mul19_out1
            .div((constant98_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub5_out1 = div12_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat13_out1 = burn::tensor::Tensor::cat([sub4_out1, sub5_out1].into(), 3);
        let gridsample2_out1 = {
            let data = div9_out1.clone();
            let dtype = data.dtype();
            data.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat13_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape15_out1 = gridsample2_out1.reshape([1, 54, 96, -1]);
        let transpose12_out1 = reshape15_out1.permute([0, 3, 1, 2]);
        let constant99_out1 = self.constant99.val();
        let constant100_out1 = self.constant100.val();
        let concat14_out1 = burn::tensor::Tensor::cat(
            [
                transpose12_out1,
                conv2d24_out1.clone(),
                constant99_out1,
                mul13_out1.clone(),
                constant100_out1.clone(),
            ]
                .into(),
            1,
        );
        let conv2d25_out1 = self.conv2d25.forward(concat14_out1);
        let leakyrelu20_out1 = burn::tensor::activation::leaky_relu(
            conv2d25_out1,
            0.10000000149011612,
        );
        let conv2d26_out1 = self.conv2d26.forward(leakyrelu20_out1);
        let leakyrelu21_out1 = burn::tensor::activation::leaky_relu(
            conv2d26_out1,
            0.10000000149011612,
        );
        let conv2d27_out1 = self.conv2d27.forward(leakyrelu21_out1);
        let leakyrelu22_out1 = burn::tensor::activation::leaky_relu(
            conv2d27_out1,
            0.10000000149011612,
        );
        let conv2d28_out1 = self.conv2d28.forward(leakyrelu22_out1);
        let leakyrelu23_out1 = burn::tensor::activation::leaky_relu(
            conv2d28_out1,
            0.10000000149011612,
        );
        let conv2d29_out1 = self.conv2d29.forward(leakyrelu23_out1);
        let leakyrelu24_out1 = burn::tensor::activation::leaky_relu(
            conv2d29_out1,
            0.10000000149011612,
        );
        let conv2d30_out1 = self.conv2d30.forward(leakyrelu24_out1);
        let leakyrelu25_out1 = burn::tensor::activation::leaky_relu(
            conv2d30_out1,
            0.10000000149011612,
        );
        let conv2d31_out1 = self.conv2d31.forward(leakyrelu25_out1);
        let leakyrelu26_out1 = burn::tensor::activation::leaky_relu(
            conv2d31_out1,
            0.10000000149011612,
        );
        let conv2d32_out1 = self.conv2d32.forward(leakyrelu26_out1);
        let slice12_out1 = conv2d32_out1.clone().slice(s![.., 2.., .., ..]);
        let clip1_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice12_out1.clamp(__clip_min, __clip_max)
        };
        let slice13_out1 = conv2d32_out1.slice(s![.., 0..2, .., ..]);
        let add12_out1 = mul13_out1.add(slice13_out1);
        let add13_out1 = constant95_out1.clone().add(add12_out1.clone());
        let transpose13_out1 = add13_out1.permute([0, 2, 3, 1]);
        let reshape16_out1 = transpose13_out1.reshape([5184, 1, 1, 2]);
        let add14_out1 = reshape16_out1.add(constant96_out1.clone());
        let split_tensors = add14_out1.split_with_sizes([1, 1].into(), 3);
        let [split5_out1, split5_out2] = split_tensors.try_into().unwrap();
        let mul20_out1 = split5_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div13_out1 = mul20_out1
            .div((constant97_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub6_out1 = div13_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul21_out1 = split5_out2
            .mul((constant90_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div14_out1 = mul21_out1
            .div((constant98_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub7_out1 = div14_out1
            .sub((constant82_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat15_out1 = burn::tensor::Tensor::cat([sub6_out1, sub7_out1].into(), 3);
        let gridsample3_out1 = {
            let dtype = div9_out1.dtype();
            div9_out1.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat15_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape17_out1 = gridsample3_out1.reshape([1, 54, 96, -1]);
        let transpose14_out1 = reshape17_out1.permute([0, 3, 1, 2]);
        let concat16_out1 = burn::tensor::Tensor::cat(
            [
                transpose14_out1,
                conv2d24_out1.clone(),
                clip1_out1,
                add12_out1.clone(),
                constant100_out1.clone(),
            ]
                .into(),
            1,
        );
        let conv2d33_out1 = self.conv2d33.forward(concat16_out1);
        let leakyrelu27_out1 = burn::tensor::activation::leaky_relu(
            conv2d33_out1,
            0.10000000149011612,
        );
        let conv2d34_out1 = self.conv2d34.forward(leakyrelu27_out1);
        let leakyrelu28_out1 = burn::tensor::activation::leaky_relu(
            conv2d34_out1,
            0.10000000149011612,
        );
        let conv2d35_out1 = self.conv2d35.forward(leakyrelu28_out1);
        let leakyrelu29_out1 = burn::tensor::activation::leaky_relu(
            conv2d35_out1,
            0.10000000149011612,
        );
        (
            leakyrelu29_out1,
            add12_out1,
            constant95_out1,
            constant96_out1,
            constant97_out1,
            constant98_out1,
            conv2d24_out1,
            constant100_out1,
        )
    }
}
#[derive(Module, Debug)]
pub struct Submodule4<B: Backend> {
    conv2d36: Conv2d<B>,
    conv2d37: Conv2d<B>,
    conv2d38: Conv2d<B>,
    conv2d39: Conv2d<B>,
    conv2d40: Conv2d<B>,
    conv2d41: Conv2d<B>,
    conv2d42: Conv2d<B>,
    conv2d43: Conv2d<B>,
    conv2d44: Conv2d<B>,
    conv2d45: Conv2d<B>,
    conv2d46: Conv2d<B>,
    conv2d47: Conv2d<B>,
    conv2d48: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule4<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d36 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d37 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d38 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d39 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d40 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d41 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d42 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d43 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d44 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d45 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d46 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d47 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d48 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            conv2d36,
            conv2d37,
            conv2d38,
            conv2d39,
            conv2d40,
            conv2d41,
            conv2d42,
            conv2d43,
            conv2d44,
            conv2d45,
            conv2d46,
            conv2d47,
            conv2d48,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        leakyrelu29_out1: Tensor<B, 4>,
        add12_out1: Tensor<B, 4>,
        constant95_out1: Tensor<B, 4>,
        constant96_out1: Tensor<B, 4>,
        constant90_out1: Tensor<B, 1>,
        constant97_out1: Tensor<B, 1>,
        constant82_out1: Tensor<B, 1>,
        constant98_out1: Tensor<B, 1>,
        div9_out1: Tensor<B, 4>,
        conv2d24_out1: Tensor<B, 4>,
        constant100_out1: Tensor<B, 4>,
    ) -> (Tensor<B, 4>, Tensor<B, 4>) {
        let conv2d36_out1 = self.conv2d36.forward(leakyrelu29_out1);
        let leakyrelu30_out1 = burn::tensor::activation::leaky_relu(
            conv2d36_out1,
            0.10000000149011612,
        );
        let conv2d37_out1 = self.conv2d37.forward(leakyrelu30_out1);
        let leakyrelu31_out1 = burn::tensor::activation::leaky_relu(
            conv2d37_out1,
            0.10000000149011612,
        );
        let conv2d38_out1 = self.conv2d38.forward(leakyrelu31_out1);
        let leakyrelu32_out1 = burn::tensor::activation::leaky_relu(
            conv2d38_out1,
            0.10000000149011612,
        );
        let conv2d39_out1 = self.conv2d39.forward(leakyrelu32_out1);
        let leakyrelu33_out1 = burn::tensor::activation::leaky_relu(
            conv2d39_out1,
            0.10000000149011612,
        );
        let conv2d40_out1 = self.conv2d40.forward(leakyrelu33_out1);
        let slice14_out1 = conv2d40_out1.clone().slice(s![.., 2.., .., ..]);
        let clip2_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice14_out1.clamp(__clip_min, __clip_max)
        };
        let slice15_out1 = conv2d40_out1.slice(s![.., 0..2, .., ..]);
        let add15_out1 = add12_out1.add(slice15_out1);
        let add16_out1 = constant95_out1.clone().add(add15_out1.clone());
        let transpose15_out1 = add16_out1.permute([0, 2, 3, 1]);
        let reshape18_out1 = transpose15_out1.reshape([5184, 1, 1, 2]);
        let add17_out1 = reshape18_out1.add(constant96_out1.clone());
        let split_tensors = add17_out1.split_with_sizes([1, 1].into(), 3);
        let [split6_out1, split6_out2] = split_tensors.try_into().unwrap();
        let mul22_out1 = split6_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div15_out1 = mul22_out1
            .div((constant97_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub8_out1 = div15_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul23_out1 = split6_out2
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div16_out1 = mul23_out1
            .div((constant98_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub9_out1 = div16_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat17_out1 = burn::tensor::Tensor::cat([sub8_out1, sub9_out1].into(), 3);
        let gridsample4_out1 = {
            let data = div9_out1.clone();
            let dtype = data.dtype();
            data.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat17_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape19_out1 = gridsample4_out1.reshape([1, 54, 96, -1]);
        let transpose16_out1 = reshape19_out1.permute([0, 3, 1, 2]);
        let concat18_out1 = burn::tensor::Tensor::cat(
            [
                transpose16_out1,
                conv2d24_out1.clone(),
                clip2_out1,
                add15_out1.clone(),
                constant100_out1.clone(),
            ]
                .into(),
            1,
        );
        let conv2d41_out1 = self.conv2d41.forward(concat18_out1);
        let leakyrelu34_out1 = burn::tensor::activation::leaky_relu(
            conv2d41_out1,
            0.10000000149011612,
        );
        let conv2d42_out1 = self.conv2d42.forward(leakyrelu34_out1);
        let leakyrelu35_out1 = burn::tensor::activation::leaky_relu(
            conv2d42_out1,
            0.10000000149011612,
        );
        let conv2d43_out1 = self.conv2d43.forward(leakyrelu35_out1);
        let leakyrelu36_out1 = burn::tensor::activation::leaky_relu(
            conv2d43_out1,
            0.10000000149011612,
        );
        let conv2d44_out1 = self.conv2d44.forward(leakyrelu36_out1);
        let leakyrelu37_out1 = burn::tensor::activation::leaky_relu(
            conv2d44_out1,
            0.10000000149011612,
        );
        let conv2d45_out1 = self.conv2d45.forward(leakyrelu37_out1);
        let leakyrelu38_out1 = burn::tensor::activation::leaky_relu(
            conv2d45_out1,
            0.10000000149011612,
        );
        let conv2d46_out1 = self.conv2d46.forward(leakyrelu38_out1);
        let leakyrelu39_out1 = burn::tensor::activation::leaky_relu(
            conv2d46_out1,
            0.10000000149011612,
        );
        let conv2d47_out1 = self.conv2d47.forward(leakyrelu39_out1);
        let leakyrelu40_out1 = burn::tensor::activation::leaky_relu(
            conv2d47_out1,
            0.10000000149011612,
        );
        let conv2d48_out1 = self.conv2d48.forward(leakyrelu40_out1);
        let slice16_out1 = conv2d48_out1.clone().slice(s![.., 2.., .., ..]);
        let clip3_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice16_out1.clamp(__clip_min, __clip_max)
        };
        let slice17_out1 = conv2d48_out1.slice(s![.., 0..2, .., ..]);
        let add18_out1 = add15_out1.add(slice17_out1);
        let add19_out1 = constant95_out1.add(add18_out1.clone());
        let transpose17_out1 = add19_out1.permute([0, 2, 3, 1]);
        let reshape20_out1 = transpose17_out1.reshape([5184, 1, 1, 2]);
        let add20_out1 = reshape20_out1.add(constant96_out1);
        let split_tensors = add20_out1.split_with_sizes([1, 1].into(), 3);
        let [split7_out1, split7_out2] = split_tensors.try_into().unwrap();
        let mul24_out1 = split7_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div17_out1 = mul24_out1
            .div((constant97_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub10_out1 = div17_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul25_out1 = split7_out2
            .mul((constant90_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div18_out1 = mul25_out1
            .div((constant98_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub11_out1 = div18_out1
            .sub((constant82_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat19_out1 = burn::tensor::Tensor::cat(
            [sub10_out1, sub11_out1].into(),
            3,
        );
        let gridsample5_out1 = {
            let dtype = div9_out1.dtype();
            div9_out1.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat19_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape21_out1 = gridsample5_out1.reshape([1, 54, 96, -1]);
        let transpose18_out1 = reshape21_out1.permute([0, 3, 1, 2]);
        let concat20_out1 = burn::tensor::Tensor::cat(
            [
                transpose18_out1,
                conv2d24_out1,
                clip3_out1,
                add18_out1.clone(),
                constant100_out1,
            ]
                .into(),
            1,
        );
        (concat20_out1, add18_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule5<B: Backend> {
    conv2d49: Conv2d<B>,
    conv2d50: Conv2d<B>,
    conv2d51: Conv2d<B>,
    conv2d52: Conv2d<B>,
    conv2d53: Conv2d<B>,
    conv2d54: Conv2d<B>,
    conv2d55: Conv2d<B>,
    conv2d56: Conv2d<B>,
    conv2d57: Conv2d<B>,
    conv2d58: Conv2d<B>,
    conv2d59: Conv2d<B>,
    conv2d60: Conv2d<B>,
    conv2d61: Conv2d<B>,
    conv2d62: Conv2d<B>,
    conv2d63: Conv2d<B>,
    conv2d64: Conv2d<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule5<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d49 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d50 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d51 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d52 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d53 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d54 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d55 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d56 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d57 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d58 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d59 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d60 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d61 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d62 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d63 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d64 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        Self {
            conv2d49,
            conv2d50,
            conv2d51,
            conv2d52,
            conv2d53,
            conv2d54,
            conv2d55,
            conv2d56,
            conv2d57,
            conv2d58,
            conv2d59,
            conv2d60,
            conv2d61,
            conv2d62,
            conv2d63,
            conv2d64,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        concat20_out1: Tensor<B, 4>,
        add18_out1: Tensor<B, 4>,
        constant95_out1: Tensor<B, 4>,
        constant96_out1: Tensor<B, 4>,
        constant90_out1: Tensor<B, 1>,
        constant97_out1: Tensor<B, 1>,
        constant82_out1: Tensor<B, 1>,
        constant98_out1: Tensor<B, 1>,
        div9_out1: Tensor<B, 4>,
        conv2d24_out1: Tensor<B, 4>,
        constant100_out1: Tensor<B, 4>,
    ) -> (Tensor<B, 4>, Tensor<B, 4>) {
        let conv2d49_out1 = self.conv2d49.forward(concat20_out1);
        let leakyrelu41_out1 = burn::tensor::activation::leaky_relu(
            conv2d49_out1,
            0.10000000149011612,
        );
        let conv2d50_out1 = self.conv2d50.forward(leakyrelu41_out1);
        let leakyrelu42_out1 = burn::tensor::activation::leaky_relu(
            conv2d50_out1,
            0.10000000149011612,
        );
        let conv2d51_out1 = self.conv2d51.forward(leakyrelu42_out1);
        let leakyrelu43_out1 = burn::tensor::activation::leaky_relu(
            conv2d51_out1,
            0.10000000149011612,
        );
        let conv2d52_out1 = self.conv2d52.forward(leakyrelu43_out1);
        let leakyrelu44_out1 = burn::tensor::activation::leaky_relu(
            conv2d52_out1,
            0.10000000149011612,
        );
        let conv2d53_out1 = self.conv2d53.forward(leakyrelu44_out1);
        let leakyrelu45_out1 = burn::tensor::activation::leaky_relu(
            conv2d53_out1,
            0.10000000149011612,
        );
        let conv2d54_out1 = self.conv2d54.forward(leakyrelu45_out1);
        let leakyrelu46_out1 = burn::tensor::activation::leaky_relu(
            conv2d54_out1,
            0.10000000149011612,
        );
        let conv2d55_out1 = self.conv2d55.forward(leakyrelu46_out1);
        let leakyrelu47_out1 = burn::tensor::activation::leaky_relu(
            conv2d55_out1,
            0.10000000149011612,
        );
        let conv2d56_out1 = self.conv2d56.forward(leakyrelu47_out1);
        let slice18_out1 = conv2d56_out1.clone().slice(s![.., 2.., .., ..]);
        let clip4_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice18_out1.clamp(__clip_min, __clip_max)
        };
        let slice19_out1 = conv2d56_out1.slice(s![.., 0..2, .., ..]);
        let add21_out1 = add18_out1.add(slice19_out1);
        let add22_out1 = constant95_out1.clone().add(add21_out1.clone());
        let transpose19_out1 = add22_out1.permute([0, 2, 3, 1]);
        let reshape22_out1 = transpose19_out1.reshape([5184, 1, 1, 2]);
        let add23_out1 = reshape22_out1.add(constant96_out1.clone());
        let split_tensors = add23_out1.split_with_sizes([1, 1].into(), 3);
        let [split8_out1, split8_out2] = split_tensors.try_into().unwrap();
        let mul26_out1 = split8_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div19_out1 = mul26_out1
            .div((constant97_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub12_out1 = div19_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul27_out1 = split8_out2
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div20_out1 = mul27_out1
            .div((constant98_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub13_out1 = div20_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat21_out1 = burn::tensor::Tensor::cat(
            [sub12_out1, sub13_out1].into(),
            3,
        );
        let gridsample6_out1 = {
            let data = div9_out1.clone();
            let dtype = data.dtype();
            data.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat21_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape23_out1 = gridsample6_out1.reshape([1, 54, 96, -1]);
        let transpose20_out1 = reshape23_out1.permute([0, 3, 1, 2]);
        let concat22_out1 = burn::tensor::Tensor::cat(
            [
                transpose20_out1,
                conv2d24_out1.clone(),
                clip4_out1,
                add21_out1.clone(),
                constant100_out1.clone(),
            ]
                .into(),
            1,
        );
        let conv2d57_out1 = self.conv2d57.forward(concat22_out1);
        let leakyrelu48_out1 = burn::tensor::activation::leaky_relu(
            conv2d57_out1,
            0.10000000149011612,
        );
        let conv2d58_out1 = self.conv2d58.forward(leakyrelu48_out1);
        let leakyrelu49_out1 = burn::tensor::activation::leaky_relu(
            conv2d58_out1,
            0.10000000149011612,
        );
        let conv2d59_out1 = self.conv2d59.forward(leakyrelu49_out1);
        let leakyrelu50_out1 = burn::tensor::activation::leaky_relu(
            conv2d59_out1,
            0.10000000149011612,
        );
        let conv2d60_out1 = self.conv2d60.forward(leakyrelu50_out1);
        let leakyrelu51_out1 = burn::tensor::activation::leaky_relu(
            conv2d60_out1,
            0.10000000149011612,
        );
        let conv2d61_out1 = self.conv2d61.forward(leakyrelu51_out1);
        let leakyrelu52_out1 = burn::tensor::activation::leaky_relu(
            conv2d61_out1,
            0.10000000149011612,
        );
        let conv2d62_out1 = self.conv2d62.forward(leakyrelu52_out1);
        let leakyrelu53_out1 = burn::tensor::activation::leaky_relu(
            conv2d62_out1,
            0.10000000149011612,
        );
        let conv2d63_out1 = self.conv2d63.forward(leakyrelu53_out1);
        let leakyrelu54_out1 = burn::tensor::activation::leaky_relu(
            conv2d63_out1,
            0.10000000149011612,
        );
        let conv2d64_out1 = self.conv2d64.forward(leakyrelu54_out1);
        let slice20_out1 = conv2d64_out1.clone().slice(s![.., 2.., .., ..]);
        let clip5_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice20_out1.clamp(__clip_min, __clip_max)
        };
        let slice21_out1 = conv2d64_out1.slice(s![.., 0..2, .., ..]);
        let add24_out1 = add21_out1.add(slice21_out1);
        let add25_out1 = constant95_out1.add(add24_out1.clone());
        let transpose21_out1 = add25_out1.permute([0, 2, 3, 1]);
        let reshape24_out1 = transpose21_out1.reshape([5184, 1, 1, 2]);
        let add26_out1 = reshape24_out1.add(constant96_out1);
        let split_tensors = add26_out1.split_with_sizes([1, 1].into(), 3);
        let [split9_out1, split9_out2] = split_tensors.try_into().unwrap();
        let mul28_out1 = split9_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div21_out1 = mul28_out1
            .div((constant97_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub14_out1 = div21_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul29_out1 = split9_out2
            .mul((constant90_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div22_out1 = mul29_out1
            .div((constant98_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub15_out1 = div22_out1
            .sub((constant82_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat23_out1 = burn::tensor::Tensor::cat(
            [sub14_out1, sub15_out1].into(),
            3,
        );
        let gridsample7_out1 = {
            let dtype = div9_out1.dtype();
            div9_out1.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat23_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape25_out1 = gridsample7_out1.reshape([1, 54, 96, -1]);
        let transpose22_out1 = reshape25_out1.permute([0, 3, 1, 2]);
        let concat24_out1 = burn::tensor::Tensor::cat(
            [
                transpose22_out1,
                conv2d24_out1,
                clip5_out1,
                add24_out1.clone(),
                constant100_out1,
            ]
                .into(),
            1,
        );
        (concat24_out1, add24_out1)
    }
}
#[derive(Module, Debug)]
pub struct Submodule6<B: Backend> {
    conv2d65: Conv2d<B>,
    conv2d66: Conv2d<B>,
    conv2d67: Conv2d<B>,
    conv2d68: Conv2d<B>,
    conv2d69: Conv2d<B>,
    conv2d70: Conv2d<B>,
    conv2d71: Conv2d<B>,
    conv2d72: Conv2d<B>,
    conv2d73: Conv2d<B>,
    conv2d74: Conv2d<B>,
    conv2d75: Conv2d<B>,
    conv2d76: Conv2d<B>,
    conv2d77: Conv2d<B>,
    conv2d78: Conv2d<B>,
    conv2d79: Conv2d<B>,
    conv2d80: Conv2d<B>,
    conv2d81: Conv2d<B>,
    conv2d82: Conv2d<B>,
    conv2d83: Conv2d<B>,
    conv2d84: Conv2d<B>,
    conv2d85: Conv2d<B>,
    conv2d86: Conv2d<B>,
    conv2d87: Conv2d<B>,
    conv2d88: Conv2d<B>,
    conv2d89: Conv2d<B>,
    conv2d90: Conv2d<B>,
    conv2d91: Conv2d<B>,
    conv2d92: Conv2d<B>,
    conv2d93: Conv2d<B>,
    constant137: burn::module::Param<Tensor<B, 2, Int>>,
    constant138: burn::module::Param<Tensor<B, 2, Int>>,
    constant104: burn::module::Param<Tensor<B, 1>>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}
impl<B: Backend> Submodule6<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let conv2d65 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d66 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d67 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d68 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d69 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d70 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d71 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d72 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d73 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d74 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d75 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d76 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d77 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d78 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d79 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d80 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d81 = Conv2dConfig::new([212, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d82 = Conv2dConfig::new([128, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d83 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d84 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d85 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d86 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d87 = Conv2dConfig::new([96, 96], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(false)
            .init(device);
        let conv2d88 = Conv2dConfig::new([96, 66], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d89 = Conv2dConfig::new([3, 128], [8, 8])
            .with_stride([8, 8])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d90 = Conv2dConfig::new([128, 128], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d91 = Conv2dConfig::new([130, 256], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d92 = Conv2dConfig::new([256, 512], [3, 3])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let conv2d93 = Conv2dConfig::new([512, 576], [1, 1])
            .with_stride([1, 1])
            .with_padding(PaddingConfig2d::Valid)
            .with_dilation([1, 1])
            .with_groups(1)
            .with_bias(true)
            .init(device);
        let constant137: burn::module::Param<Tensor<B, 2, Int>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                2,
                Int,
            >::zeros([3, 54], (device, burn::tensor::DType::I64)),
            device.clone(),
            false,
            [3, 54].into(),
        );
        let constant138: burn::module::Param<Tensor<B, 2, Int>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                2,
                Int,
            >::zeros([3, 96], (device, burn::tensor::DType::I64)),
            device.clone(),
            false,
            [3, 96].into(),
        );
        let constant104: burn::module::Param<Tensor<B, 1>> = burn::module::Param::uninitialized(
            burn::module::ParamId::new(),
            move |device, _require_grad| Tensor::<
                B,
                1,
            >::from_data(
                burn::tensor::TensorData::from([8f64]),
                (device, burn::tensor::DType::F16),
            ),
            device.clone(),
            false,
            [1].into(),
        );
        Self {
            conv2d65,
            conv2d66,
            conv2d67,
            conv2d68,
            conv2d69,
            conv2d70,
            conv2d71,
            conv2d72,
            conv2d73,
            conv2d74,
            conv2d75,
            conv2d76,
            conv2d77,
            conv2d78,
            conv2d79,
            conv2d80,
            conv2d81,
            conv2d82,
            conv2d83,
            conv2d84,
            conv2d85,
            conv2d86,
            conv2d87,
            conv2d88,
            conv2d89,
            conv2d90,
            conv2d91,
            conv2d92,
            conv2d93,
            constant137,
            constant138,
            constant104,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }
    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(
        &self,
        concat24_out1: Tensor<B, 4>,
        add24_out1: Tensor<B, 4>,
        constant95_out1: Tensor<B, 4>,
        constant96_out1: Tensor<B, 4>,
        constant90_out1: Tensor<B, 1>,
        constant97_out1: Tensor<B, 1>,
        constant82_out1: Tensor<B, 1>,
        constant98_out1: Tensor<B, 1>,
        div9_out1: Tensor<B, 4>,
        conv2d24_out1: Tensor<B, 4>,
        constant100_out1: Tensor<B, 4>,
        div1_out1: Tensor<B, 4>,
    ) -> Tensor<B, 4> {
        let conv2d65_out1 = self.conv2d65.forward(concat24_out1);
        let leakyrelu55_out1 = burn::tensor::activation::leaky_relu(
            conv2d65_out1,
            0.10000000149011612,
        );
        let conv2d66_out1 = self.conv2d66.forward(leakyrelu55_out1);
        let leakyrelu56_out1 = burn::tensor::activation::leaky_relu(
            conv2d66_out1,
            0.10000000149011612,
        );
        let conv2d67_out1 = self.conv2d67.forward(leakyrelu56_out1);
        let leakyrelu57_out1 = burn::tensor::activation::leaky_relu(
            conv2d67_out1,
            0.10000000149011612,
        );
        let conv2d68_out1 = self.conv2d68.forward(leakyrelu57_out1);
        let leakyrelu58_out1 = burn::tensor::activation::leaky_relu(
            conv2d68_out1,
            0.10000000149011612,
        );
        let conv2d69_out1 = self.conv2d69.forward(leakyrelu58_out1);
        let leakyrelu59_out1 = burn::tensor::activation::leaky_relu(
            conv2d69_out1,
            0.10000000149011612,
        );
        let conv2d70_out1 = self.conv2d70.forward(leakyrelu59_out1);
        let leakyrelu60_out1 = burn::tensor::activation::leaky_relu(
            conv2d70_out1,
            0.10000000149011612,
        );
        let conv2d71_out1 = self.conv2d71.forward(leakyrelu60_out1);
        let leakyrelu61_out1 = burn::tensor::activation::leaky_relu(
            conv2d71_out1,
            0.10000000149011612,
        );
        let conv2d72_out1 = self.conv2d72.forward(leakyrelu61_out1);
        let slice22_out1 = conv2d72_out1.clone().slice(s![.., 2.., .., ..]);
        let clip6_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice22_out1.clamp(__clip_min, __clip_max)
        };
        let slice23_out1 = conv2d72_out1.slice(s![.., 0..2, .., ..]);
        let add27_out1 = add24_out1.add(slice23_out1);
        let add28_out1 = constant95_out1.clone().add(add27_out1.clone());
        let transpose23_out1 = add28_out1.permute([0, 2, 3, 1]);
        let reshape26_out1 = transpose23_out1.reshape([5184, 1, 1, 2]);
        let add29_out1 = reshape26_out1.add(constant96_out1.clone());
        let split_tensors = add29_out1.split_with_sizes([1, 1].into(), 3);
        let [split10_out1, split10_out2] = split_tensors.try_into().unwrap();
        let mul30_out1 = split10_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div23_out1 = mul30_out1
            .div((constant97_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub16_out1 = div23_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul31_out1 = split10_out2
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div24_out1 = mul31_out1
            .div((constant98_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub17_out1 = div24_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat25_out1 = burn::tensor::Tensor::cat(
            [sub16_out1, sub17_out1].into(),
            3,
        );
        let gridsample8_out1 = {
            let data = div9_out1.clone();
            let dtype = data.dtype();
            data.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat25_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape27_out1 = gridsample8_out1.reshape([1, 54, 96, -1]);
        let transpose24_out1 = reshape27_out1.permute([0, 3, 1, 2]);
        let concat26_out1 = burn::tensor::Tensor::cat(
            [
                transpose24_out1,
                conv2d24_out1.clone(),
                clip6_out1,
                add27_out1.clone(),
                constant100_out1.clone(),
            ]
                .into(),
            1,
        );
        let conv2d73_out1 = self.conv2d73.forward(concat26_out1);
        let leakyrelu62_out1 = burn::tensor::activation::leaky_relu(
            conv2d73_out1,
            0.10000000149011612,
        );
        let conv2d74_out1 = self.conv2d74.forward(leakyrelu62_out1);
        let leakyrelu63_out1 = burn::tensor::activation::leaky_relu(
            conv2d74_out1,
            0.10000000149011612,
        );
        let conv2d75_out1 = self.conv2d75.forward(leakyrelu63_out1);
        let leakyrelu64_out1 = burn::tensor::activation::leaky_relu(
            conv2d75_out1,
            0.10000000149011612,
        );
        let conv2d76_out1 = self.conv2d76.forward(leakyrelu64_out1);
        let leakyrelu65_out1 = burn::tensor::activation::leaky_relu(
            conv2d76_out1,
            0.10000000149011612,
        );
        let conv2d77_out1 = self.conv2d77.forward(leakyrelu65_out1);
        let leakyrelu66_out1 = burn::tensor::activation::leaky_relu(
            conv2d77_out1,
            0.10000000149011612,
        );
        let conv2d78_out1 = self.conv2d78.forward(leakyrelu66_out1);
        let leakyrelu67_out1 = burn::tensor::activation::leaky_relu(
            conv2d78_out1,
            0.10000000149011612,
        );
        let conv2d79_out1 = self.conv2d79.forward(leakyrelu67_out1);
        let leakyrelu68_out1 = burn::tensor::activation::leaky_relu(
            conv2d79_out1,
            0.10000000149011612,
        );
        let conv2d80_out1 = self.conv2d80.forward(leakyrelu68_out1);
        let slice24_out1 = conv2d80_out1.clone().slice(s![.., 2.., .., ..]);
        let clip7_out1 = {
            let __clip_min = -4f64;
            let __clip_max = 4f64;
            slice24_out1.clamp(__clip_min, __clip_max)
        };
        let slice25_out1 = conv2d80_out1.slice(s![.., 0..2, .., ..]);
        let add30_out1 = add27_out1.add(slice25_out1);
        let add31_out1 = constant95_out1.add(add30_out1.clone());
        let transpose25_out1 = add31_out1.permute([0, 2, 3, 1]);
        let reshape28_out1 = transpose25_out1.reshape([5184, 1, 1, 2]);
        let add32_out1 = reshape28_out1.add(constant96_out1);
        let split_tensors = add32_out1.split_with_sizes([1, 1].into(), 3);
        let [split11_out1, split11_out2] = split_tensors.try_into().unwrap();
        let mul32_out1 = split11_out1
            .mul((constant90_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div25_out1 = mul32_out1
            .div((constant97_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub18_out1 = div25_out1
            .sub((constant82_out1.clone()).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let mul33_out1 = split11_out2
            .mul((constant90_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let div26_out1 = mul33_out1
            .div((constant98_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let sub19_out1 = div26_out1
            .sub((constant82_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        let concat27_out1 = burn::tensor::Tensor::cat(
            [sub18_out1, sub19_out1].into(),
            3,
        );
        let gridsample9_out1 = {
            let dtype = div9_out1.dtype();
            div9_out1.cast(burn::tensor::DType::F32)
            .grid_sample_2d(
                concat27_out1.cast(burn::tensor::DType::F32),
                burn::tensor::ops::GridSampleOptions::new(
                        burn::tensor::ops::InterpolateMode::Bilinear,
                    )
                    .with_padding_mode(burn::tensor::ops::GridSamplePaddingMode::Zeros)
                    .with_align_corners(true),
            )
            .cast(dtype)
        };
        let reshape29_out1 = gridsample9_out1.reshape([1, 54, 96, -1]);
        let transpose26_out1 = reshape29_out1.permute([0, 3, 1, 2]);
        let concat28_out1 = burn::tensor::Tensor::cat(
            [
                transpose26_out1,
                conv2d24_out1,
                clip7_out1,
                add30_out1.clone(),
                constant100_out1,
            ]
                .into(),
            1,
        );
        let conv2d81_out1 = self.conv2d81.forward(concat28_out1);
        let leakyrelu69_out1 = burn::tensor::activation::leaky_relu(
            conv2d81_out1,
            0.10000000149011612,
        );
        let conv2d82_out1 = self.conv2d82.forward(leakyrelu69_out1);
        let leakyrelu70_out1 = burn::tensor::activation::leaky_relu(
            conv2d82_out1,
            0.10000000149011612,
        );
        let conv2d83_out1 = self.conv2d83.forward(leakyrelu70_out1);
        let leakyrelu71_out1 = burn::tensor::activation::leaky_relu(
            conv2d83_out1,
            0.10000000149011612,
        );
        let conv2d84_out1 = self.conv2d84.forward(leakyrelu71_out1);
        let leakyrelu72_out1 = burn::tensor::activation::leaky_relu(
            conv2d84_out1,
            0.10000000149011612,
        );
        let conv2d85_out1 = self.conv2d85.forward(leakyrelu72_out1);
        let leakyrelu73_out1 = burn::tensor::activation::leaky_relu(
            conv2d85_out1,
            0.10000000149011612,
        );
        let conv2d86_out1 = self.conv2d86.forward(leakyrelu73_out1);
        let leakyrelu74_out1 = burn::tensor::activation::leaky_relu(
            conv2d86_out1,
            0.10000000149011612,
        );
        let conv2d87_out1 = self.conv2d87.forward(leakyrelu74_out1);
        let leakyrelu75_out1 = burn::tensor::activation::leaky_relu(
            conv2d87_out1,
            0.10000000149011612,
        );
        let conv2d88_out1 = self.conv2d88.forward(leakyrelu75_out1);
        let slice26_out1 = conv2d88_out1.slice(s![.., 0..2, .., ..]);
        let add33_out1 = add30_out1.add(slice26_out1);
        let conv2d89_out1 = self.conv2d89.forward(div1_out1);
        let leakyrelu76_out1 = burn::tensor::activation::leaky_relu(
            conv2d89_out1,
            0.10000000149011612,
        );
        let conv2d90_out1 = self.conv2d90.forward(leakyrelu76_out1);
        let leakyrelu77_out1 = burn::tensor::activation::leaky_relu(
            conv2d90_out1,
            0.10000000149011612,
        );
        let concat29_out1 = burn::tensor::Tensor::cat(
            [add33_out1.clone(), leakyrelu77_out1].into(),
            1,
        );
        let conv2d91_out1 = self.conv2d91.forward(concat29_out1);
        let relu3_out1 = burn::tensor::activation::relu(conv2d91_out1);
        let conv2d92_out1 = self.conv2d92.forward(relu3_out1);
        let relu4_out1 = burn::tensor::activation::relu(conv2d92_out1);
        let conv2d93_out1 = self.conv2d93.forward(relu4_out1);
        let reshape30_out1 = conv2d93_out1.reshape([1, 1, 9, 8, 8, 54, 96]);
        let softmax4_out1 = {
            let dtype = reshape30_out1.dtype();
            burn::tensor::activation::softmax(reshape30_out1.cast(burn::tensor::DType::F32), 2)
                .cast(dtype)
        };
        let pad1_out1 = add33_out1
            .pad(
                [(0usize, 0usize), (0usize, 0usize), (1usize, 1usize), (1usize, 1usize)],
                burn::tensor::ops::PadMode::Constant(0_f32),
            );
        let constant137_out1 = self.constant137.val();
        let gather1_out1 = pad1_out1.take::<2, 5>(2, constant137_out1);
        let constant138_out1 = self.constant138.val();
        let gather2_out1 = gather1_out1.take::<2, 6>(4, constant138_out1);
        let transpose27_out1 = gather2_out1.permute([0, 1, 2, 4, 3, 5]);
        let reshape31_out1 = transpose27_out1.reshape([1, 2, 9, 1, 1, 54, 96]);
        let mul34_out1 = softmax4_out1.mul(reshape31_out1);
        let reducesum1_out1 = {
            let dtype = mul34_out1.dtype();
            mul34_out1.cast(burn::tensor::DType::F32)
                .sum_dim(2usize).squeeze_dims::<6usize>(&[2])
                .cast(dtype)
        };
        let transpose28_out1 = reducesum1_out1.permute([0, 1, 4, 2, 5, 3]);
        let reshape32_out1 = transpose28_out1.reshape([1, 2, 432, 768]);
        let constant104_out1 = self.constant104.val();
        let mul35_out1 = reshape32_out1
            .mul((constant104_out1).unsqueeze_dims(&[0isize, 1isize, 2isize]));
        mul35_out1
    }
}

#[derive(Module, Debug)]
pub struct Model<B: Backend> {
    submodule1: Submodule1<B>,
    submodule2: Submodule2<B>,
    submodule3: Submodule3<B>,
    submodule4: Submodule4<B>,
    submodule5: Submodule5<B>,
    submodule6: Submodule6<B>,
    phantom: core::marker::PhantomData<B>,
    #[module(skip)]
    device: B::Device,
}


impl<B: Backend> Default for Model<B> {
    fn default() -> Self {
        Self::from_file(
            "C:/Users/Jhe/Desktop/github/gyroflow-burn-neuflow/src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk",
            &Default::default(),
        )
    }
}

impl<B: Backend> Model<B> {
    /// Load model weights from a burnpack file.
    pub fn from_file(file: &str, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_file(file);
        model.load_from(&mut store).expect("Failed to load burnpack file");
        model
    }

    /// Load model weights from in-memory bytes.
    ///
    /// The bytes must be the contents of a `.bpk` file.
    pub fn from_bytes(bytes: Bytes, device: &B::Device) -> Self {
        let mut model = Self::new(device);
        let mut store = BurnpackStore::from_bytes(Some(bytes));
        model.load_from(&mut store).expect("Failed to load burnpack bytes");
        model
    }
}

impl<B: Backend> Model<B> {
    #[allow(unused_variables)]
    pub fn new(device: &B::Device) -> Self {
        let submodule1 = Submodule1::new(device);
        let submodule2 = Submodule2::new(device);
        let submodule3 = Submodule3::new(device);
        let submodule4 = Submodule4::new(device);
        let submodule5 = Submodule5::new(device);
        let submodule6 = Submodule6::new(device);
        Self {
            submodule1,
            submodule2,
            submodule3,
            submodule4,
            submodule5,
            submodule6,
            phantom: core::marker::PhantomData,
            device: device.clone(),
        }
    }

    #[allow(clippy::let_and_return, clippy::approx_constant)]
    pub fn forward(&self, img0: Tensor<B, 4>, img1: Tensor<B, 4>) -> Tensor<B, 4> {
        let (
            add4_out1,
            leakyrelu6_out1,
            constant82_out1,
            constant81_out1,
            constant83_out1,
            div1_out1,
        ) = self.submodule1.forward(img0, img1);
        let (mul17_out1, mul13_out1, constant90_out1, div9_out1) = self
            .submodule2
            .forward(
                add4_out1,
                leakyrelu6_out1,
                constant82_out1.clone(),
                constant81_out1,
                constant83_out1,
            );
        let (
            leakyrelu29_out1,
            add12_out1,
            constant95_out1,
            constant96_out1,
            constant97_out1,
            constant98_out1,
            conv2d24_out1,
            constant100_out1,
        ) = self
            .submodule3
            .forward(
                mul17_out1,
                mul13_out1,
                constant90_out1.clone(),
                constant82_out1.clone(),
                div9_out1.clone(),
            );
        let (concat20_out1, add18_out1) = self
            .submodule4
            .forward(
                leakyrelu29_out1,
                add12_out1,
                constant95_out1.clone(),
                constant96_out1.clone(),
                constant90_out1.clone(),
                constant97_out1.clone(),
                constant82_out1.clone(),
                constant98_out1.clone(),
                div9_out1.clone(),
                conv2d24_out1.clone(),
                constant100_out1.clone(),
            );
        let (concat24_out1, add24_out1) = self
            .submodule5
            .forward(
                concat20_out1,
                add18_out1,
                constant95_out1.clone(),
                constant96_out1.clone(),
                constant90_out1.clone(),
                constant97_out1.clone(),
                constant82_out1.clone(),
                constant98_out1.clone(),
                div9_out1.clone(),
                conv2d24_out1.clone(),
                constant100_out1.clone(),
            );
        let mul35_out1 = self
            .submodule6
            .forward(
                concat24_out1,
                add24_out1,
                constant95_out1,
                constant96_out1,
                constant90_out1,
                constant97_out1,
                constant82_out1,
                constant98_out1,
                div9_out1,
                conv2d24_out1,
                constant100_out1,
                div1_out1,
            );
        mul35_out1
    }
}
