// https://github.com/huggingface/diffusers/blob/main/src/diffusers/models/controlnet.py
use super::unet_2d::{BlockConfig, UNetDownBlock};
use crate::models::embeddings::{TimestepEmbedding, Timesteps};
use crate::models::unet_2d_blocks::*;
use tch::{nn, Tensor};

#[derive(Debug)]
pub struct ControlNetConditioningEmbedding {
    conv_in: nn::Conv2D,
    conv_out: nn::Conv2D,
    blocks: Vec<(nn::Conv2D, nn::Conv2D)>,
}

impl ControlNetConditioningEmbedding {
    pub fn new(
        vs: nn::Path,
        conditioning_embedding_channels: i64,
        conditioning_channels: i64,
        blocks: &[BlockConfig],
    ) -> Self {
        let b_channels = blocks[0].out_channels;
        let bl_channels = blocks.last().unwrap().out_channels;
        let conv_cfg = nn::ConvConfig { padding: 1, ..Default::default() };
        let conv_cfg2 = nn::ConvConfig { stride: 2, padding: 1, ..Default::default() };
        let conv_in = nn::conv2d(&vs / "conv_in", conditioning_channels, b_channels, 3, conv_cfg);
        let conv_out =
            nn::conv2d(&vs / "conv_out", bl_channels, conditioning_embedding_channels, 3, conv_cfg);
        let vs_b = &vs / "blocks";
        let blocks = (0..(blocks.len() - 1))
            .map(|i| {
                let channel_in = blocks[i].out_channels;
                let channel_out = blocks[i + 1].out_channels;
                let c1 = nn::conv2d(&vs / (2 * i), channel_in, channel_in, 3, conv_cfg);
                let c2 = nn::conv2d(&vs / (2 * i + 1), channel_in, channel_out, 3, conv_cfg2);
                (c1, c2)
            })
            .collect();
        Self { conv_in, conv_out, blocks }
    }
}

impl tch::nn::Module for ControlNetConditioningEmbedding {
    fn forward(&self, xs: &Tensor) -> Tensor {
        let mut xs = xs.apply(&self.conv_in).silu();
        for (c1, c2) in self.blocks.iter() {
            xs = xs.apply(c1).apply(c2).silu();
        }
        xs.apply(&self.conv_out)
    }
}

pub struct ControlNetConfig {
    pub flip_sin_to_cos: bool,
    pub freq_shift: f64,
    pub blocks: Vec<BlockConfig>,
    pub layers_per_block: i64,
    pub downsample_padding: i64,
    pub mid_block_scale_factor: f64,
    pub norm_num_groups: i64,
    pub norm_eps: f64,
    pub cross_attention_dim: i64,
    pub use_linear_projection: bool,
}

#[allow(dead_code)]
pub struct ControlNet {
    conv_in: nn::Conv2D,
    controlnet_block: nn::Conv2D,
    controlnet_cond_embedding: ControlNetConditioningEmbedding,
    time_proj: Timesteps,
    time_embedding: TimestepEmbedding,
    down_blocks: Vec<UNetDownBlock>,
    mid_block: UNetMidBlock2DCrossAttn,
    pub config: ControlNetConfig,
}

impl ControlNet {
    pub fn new(vs: nn::Path, in_channels: i64, config: ControlNetConfig) -> Self {
        let n_blocks = config.blocks.len();
        let b_channels = config.blocks[0].out_channels;
        let time_embed_dim = b_channels * 4;
        let time_proj =
            Timesteps::new(b_channels, config.flip_sin_to_cos, config.freq_shift, vs.device());
        let time_embedding =
            TimestepEmbedding::new(&vs / "time_embedding", b_channels, time_embed_dim);
        let conv_cfg = nn::ConvConfig { stride: 1, padding: 1, ..Default::default() };
        let conv_in = nn::conv2d(&vs / "conv_in", in_channels, b_channels, 3, conv_cfg);
        let controlnet_block =
            nn::conv2d(&vs / "controlnet_block", b_channels, b_channels, 1, Default::default());
        let controlnet_cond_embedding = ControlNetConditioningEmbedding::new(
            &vs / "controlnet_cond_embedding",
            b_channels,
            3,
            &config.blocks,
        );
        let vs_db = &vs / "down_blocks";
        let down_blocks = (0..n_blocks)
            .map(|i| {
                let BlockConfig { out_channels, use_cross_attn, attention_head_dim } =
                    config.blocks[i];

                let in_channels =
                    if i > 0 { config.blocks[i - 1].out_channels } else { b_channels };
                let db_cfg = DownBlock2DConfig {
                    num_layers: config.layers_per_block,
                    resnet_eps: config.norm_eps,
                    resnet_groups: config.norm_num_groups,
                    add_downsample: i < n_blocks - 1,
                    downsample_padding: config.downsample_padding,
                    ..Default::default()
                };
                if use_cross_attn {
                    let config = CrossAttnDownBlock2DConfig {
                        downblock: db_cfg,
                        attn_num_head_channels: attention_head_dim,
                        cross_attention_dim: config.cross_attention_dim,
                        sliced_attention_size: None,
                        use_linear_projection: config.use_linear_projection,
                    };
                    let block = CrossAttnDownBlock2D::new(
                        &vs_db / i,
                        in_channels,
                        out_channels,
                        Some(time_embed_dim),
                        config,
                    );
                    UNetDownBlock::CrossAttn(block)
                } else {
                    let block = DownBlock2D::new(
                        &vs_db / i,
                        in_channels,
                        out_channels,
                        Some(time_embed_dim),
                        db_cfg,
                    );
                    UNetDownBlock::Basic(block)
                }
            })
            .collect();
        let bl_channels = config.blocks.last().unwrap().out_channels;
        let bl_attention_head_dim = config.blocks.last().unwrap().attention_head_dim;
        let mid_cfg = UNetMidBlock2DCrossAttnConfig {
            resnet_eps: config.norm_eps,
            output_scale_factor: config.mid_block_scale_factor,
            cross_attn_dim: config.cross_attention_dim,
            attn_num_head_channels: bl_attention_head_dim,
            resnet_groups: Some(config.norm_num_groups),
            use_linear_projection: config.use_linear_projection,
            ..Default::default()
        };
        let mid_block = UNetMidBlock2DCrossAttn::new(
            &vs / "mid_block",
            bl_channels,
            Some(time_embed_dim),
            mid_cfg,
        );

        Self {
            conv_in,
            controlnet_block,
            controlnet_cond_embedding,
            time_proj,
            time_embedding,
            down_blocks,
            mid_block,
            config,
        }
    }

    pub fn forward(&self, xs: &Tensor) -> (Tensor, Tensor) {
        let down_block_res_samples = xs.shallow_clone();
        let mid_block_res_samples = xs.shallow_clone();
        (down_block_res_samples, mid_block_res_samples)
    }
}
