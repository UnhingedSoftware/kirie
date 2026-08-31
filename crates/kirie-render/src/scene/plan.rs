use kirie_scene::material::{Blending, CullMode, DepthMode, Pass, PassCommand};
use kirie_scene::object::{ImageObject, PassOverride};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassInput {
    Layer,
    Fbo(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassOutput {
    Fbo(usize),
    Named(String),
    Scene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Geometry {
    Copy,
    SceneCopy,
    Pass,
    Scene,
    Puppet,
    PuppetCopy,
}

#[derive(Debug, Clone)]
pub struct PlanPass {
    pub shader: String,
    pub blending: Blending,
    pub cull: CullMode,
    pub depthtest: DepthMode,
    pub depthwrite: DepthMode,
    pub pass: Pass,
    pub input: PassInput,
    pub output: PassOutput,
    pub geometry: Geometry,
    pub effect_index: Option<usize>,
    pub target: Option<String>,
    pub binds: Vec<(u32, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct ImagePlan {
    pub passes: Vec<PlanPass>,
    pub named_fbos: Vec<kirie_scene::material::Fbo>,
}

fn apply_override(mut pass: Pass, ov: &PassOverride) -> Pass {
    for (k, v) in &ov.combos {
        pass.combos.insert(k.clone(), *v);
    }
    for (k, v) in &ov.constantshadervalues {
        pass.constantshadervalues.insert(k.clone(), v.clone());
    }
    for (i, slot) in ov.textures.iter().enumerate() {
        if slot.is_some() {
            if i >= pass.textures.len() {
                pass.textures.resize(i + 1, None);
            }
            pass.textures[i] = slot.clone();
        }
    }
    pass
}

struct SrcPass {
    pass: Pass,
    target: Option<String>,
    binds: Vec<(u32, String)>,
    effect_index: Option<usize>,
}

pub const COPY_COMMAND_SHADER: &str = "commands/copy";

fn copy_command_pass(source: &str, target: &str) -> SrcPass {
    SrcPass {
        effect_index: None,
        pass: Pass {
            blending: Blending::Normal,
            cullmode: CullMode::NoCull,
            depthtest: DepthMode::Disabled,
            depthwrite: DepthMode::Disabled,
            shader: COPY_COMMAND_SHADER.to_owned(),
            textures: vec![Some(source.to_owned())],
            usertextures: vec![],
            combos: Default::default(),
            constantshadervalues: Default::default(),
        },
        target: Some(target.to_owned()),
        binds: Vec::new(),
    }
}

fn base_passes(image: &ImageObject) -> Vec<SrcPass> {
    image
        .material
        .as_ref()
        .map(|m| {
            m.passes
                .iter()
                .cloned()
                .map(|pass| SrcPass {
                    pass,
                    target: None,
                    binds: Vec::new(),
                    effect_index: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn effect_passes(image: &ImageObject) -> Vec<SrcPass> {
    let mut out = Vec::new();
    for (ei, effect) in image.effects.iter().enumerate() {
        if !effect.visible.value {
            continue;
        }
        let Some(file) = &effect.resolved else { continue };
        let mut ov_next = 0usize;
        for epass in &file.passes {
            if epass.material.is_none() {
                match (&epass.command, &epass.source, &epass.target) {
                    (Some(PassCommand::Copy), Some(source), Some(target)) => {
                        let mut p = copy_command_pass(source, target);
                        p.effect_index = Some(ei);
                        out.push(p);
                    }
                    _ => {
                        tracing::debug!(
                            effect = %effect.file,
                            command = ?epass.command,
                            "unsupported effect command pass skipped (only copy; CImage.cpp:699)"
                        );
                    }
                }
                continue;
            }
            let ov = effect.passes.get(ov_next);
            ov_next += 1;
            let Some(mat) = &epass.resolved else {
                tracing::debug!(effect = %effect.file, "effect pass material unresolved; skipped");
                continue;
            };
            let binds: Vec<(u32, String)> = epass
                .bind
                .iter()
                .filter_map(|b| u32::try_from(b.index).ok().map(|i| (i, b.name.clone())))
                .collect();
            for (mi, mpass) in mat.passes.iter().enumerate() {
                out.push(SrcPass {
                    pass: match ov {
                        Some(o) => apply_override(mpass.clone(), o),
                        None => mpass.clone(),
                    },
                    target: epass.target.clone(),
                    binds: if mi == 0 { binds.clone() } else { Vec::new() },
                    effect_index: Some(ei),
                });
            }
        }
    }
    out
}

fn effect_fbos(image: &ImageObject) -> Vec<kirie_scene::material::Fbo> {
    let mut out: Vec<kirie_scene::material::Fbo> = Vec::new();
    for effect in &image.effects {
        if !effect.visible.value {
            continue;
        }
        let Some(file) = &effect.resolved else { continue };
        for fbo in &file.fbos {
            if !out.iter().any(|f| f.name == fbo.name) {
                out.push(fbo.clone());
            }
        }
    }
    out
}

pub const COLOR_BLEND_MATERIAL: &str = "materials/util/effectpassthrough.json";

#[must_use]
pub fn plan_image(
    image: &ImageObject,
    visible: bool,
    offscreen_donor: bool,
    color_blend: Option<&kirie_scene::material::Material>,
) -> ImagePlan {
    if !visible {
        return ImagePlan::default();
    }
    let passthrough = image.model.as_ref().is_some_and(|m| m.passthrough);
    let effects = effect_passes(image);
    if passthrough && effects.is_empty() {
        return ImagePlan::default();
    }
    let mut passes = base_passes(image);
    passes.extend(effects);
    if passes.is_empty() {
        return ImagePlan::default();
    }

    if image.color_blend_mode.value > 0
        && let Some(mat) = color_blend
        && let Some(first) = mat.passes.first()
    {
        let ov = PassOverride {
            id: -1,
            combos: [("BLENDMODE".to_owned(), image.color_blend_mode.value)]
                .into_iter()
                .collect(),
            constantshadervalues: Default::default(),
            textures: vec![],
            usertextures: vec![],
        };
        passes.push(SrcPass {
            pass: apply_override(first.clone(), &ov),
            target: None,
            binds: Vec::new(),
            effect_index: None,
        });
    }

    if passes.len() > 1 {
        let first_blend = passes[0].pass.blending;
        passes[0].pass.blending = Blending::Normal;
        if !offscreen_donor {
            let last = passes.len() - 1;
            passes[last].pass.blending = first_blend;
        }
    } else if offscreen_donor {
        passes[0].pass.blending = Blending::Normal;
    }

    let n = passes.len();
    let mut wired = Vec::with_capacity(n);
    let mut input = PassInput::Layer;
    let mut cur_out = 0usize;
    for (i, src) in passes.into_iter().enumerate() {
        let is_last = i == n - 1;
        let (output, geometry) = if is_last {
            (PassOutput::Scene, Geometry::Scene)
        } else if i == 0 {
            (PassOutput::Fbo(cur_out), Geometry::Copy)
        } else {
            (PassOutput::Fbo(cur_out), Geometry::Pass)
        };
        let geometry = if n == 1 { Geometry::Scene } else { geometry };
        let SrcPass {
            pass,
            target,
            binds,
            effect_index,
        } = src;
        wired.push(PlanPass {
            effect_index,
            shader: pass.shader.clone(),
            blending: pass.blending,
            cull: pass.cullmode,
            depthtest: pass.depthtest,
            depthwrite: pass.depthwrite,
            pass,
            input,
            output,
            geometry,
            target,
            binds,
        });
        if !is_last {
            input = PassInput::Fbo(cur_out);
            cur_out = 1 - cur_out;
        }
    }
    ImagePlan {
        passes: wired,
        named_fbos: effect_fbos(image),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirie_scene::material::Material;
    use kirie_scene::object::ImageObject;
    use kirie_scene::user::UserSetting;
    use kirie_scene::value::WHITE;

    fn pass(shader: &str, blending: Blending) -> Pass {
        Pass {
            blending,
            cullmode: CullMode::NoCull,
            depthtest: DepthMode::Disabled,
            depthwrite: DepthMode::Disabled,
            shader: shader.to_string(),
            textures: vec![],
            usertextures: vec![],
            combos: Default::default(),
            constantshadervalues: Default::default(),
        }
    }

    fn image(passes: Vec<Pass>) -> ImageObject {
        ImageObject {
            image: "img.json".into(),
            model: None,
            material: Some(Material { passes }),
            scale: UserSetting::literal([1.0, 1.0, 1.0]),
            angles: UserSetting::literal([0.0, 0.0, 0.0]),
            visible: UserSetting::literal(true),
            alpha: UserSetting::literal(1.0),
            color: UserSetting::literal(WHITE),
            alignment: "center".into(),
            size: [0.0, 0.0],
            parallax_depth: UserSetting::literal([0.0, 0.0]),
            color_blend_mode: UserSetting::literal(0),
            brightness: UserSetting::literal(1.0),
            effects: vec![],
            animationlayers: vec![],
            instance: None,
        }
    }

    fn passthrough_model() -> kirie_scene::material::ModelFile {
        kirie_scene::material::ModelFile {
            material: "materials/util/fullscreenlayer.json".into(),
            solidlayer: false,
            fullscreen: true,
            passthrough: true,
            autosize: false,
            nopadding: false,
            width: None,
            height: None,
            puppet: None,
        }
    }

    #[test]
    fn hidden_image_plans_nothing() {
        let img = image(vec![pass("effect", Blending::Normal)]);
        assert!(plan_image(&img, false, false, None).passes.is_empty());
    }

    #[test]
    fn passthrough_layer_without_visible_effects_is_skipped() {
        let mut img = image(vec![pass("passthrough", Blending::Translucent)]);
        img.model = Some(passthrough_model());
        assert!(plan_image(&img, true, false, None).passes.is_empty());
    }

    #[test]
    fn passthrough_layer_with_visible_effect_renders() {
        use kirie_scene::material::{EffectFile, EffectPass};
        use kirie_scene::object::Effect;
        use kirie_scene::user::UserSetting;
        let mut img = image(vec![pass("passthrough", Blending::Translucent)]);
        img.model = Some(passthrough_model());
        img.effects = vec![Effect {
            file: "effects/tint/effect.json".into(),
            id: -1,
            name: "tint".into(),
            visible: UserSetting::literal(true),
            passes: vec![],
            resolved: Some(EffectFile {
                name: String::new(),
                description: String::new(),
                group: String::new(),
                preview: String::new(),
                dependencies: vec![],
                fbos: vec![],
                passes: vec![EffectPass {
                    material: Some("materials/effects/tint.json".into()),
                    resolved: Some(Material {
                        passes: vec![pass("effects/tint", Blending::Normal)],
                    }),
                    bind: vec![],
                    command: None,
                    source: None,
                    target: None,
                }],
            }),
        }];
        assert_eq!(plan_image(&img, true, false, None).passes.len(), 2);
    }

    #[test]
    fn image_without_material_plans_nothing() {
        let mut img = image(vec![]);
        img.material = None;
        assert!(plan_image(&img, true, false, None).passes.is_empty());
    }

    #[test]
    fn single_pass_composites_into_scene_from_layer() {
        let img = image(vec![pass("passthrough", Blending::Translucent)]);
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 1);
        let p = &plan.passes[0];
        assert_eq!(p.input, PassInput::Layer);
        assert_eq!(p.output, PassOutput::Scene);
        assert_eq!(p.geometry, Geometry::Scene);
        assert_eq!(p.blending, Blending::Translucent);
    }

    #[test]
    fn multi_pass_ping_pongs_and_ends_at_scene() {
        let img = image(vec![
            pass("a", Blending::Additive),
            pass("b", Blending::Normal),
            pass("c", Blending::Normal),
        ]);
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 3);

        assert_eq!(plan.passes[0].input, PassInput::Layer);
        assert_eq!(plan.passes[0].output, PassOutput::Fbo(0));
        assert_eq!(plan.passes[0].geometry, Geometry::Copy);

        assert_eq!(plan.passes[1].input, PassInput::Fbo(0));
        assert_eq!(plan.passes[1].output, PassOutput::Fbo(1));
        assert_eq!(plan.passes[1].geometry, Geometry::Pass);

        assert_eq!(plan.passes[2].input, PassInput::Fbo(1));
        assert_eq!(plan.passes[2].output, PassOutput::Scene);
        assert_eq!(plan.passes[2].geometry, Geometry::Scene);
    }

    fn effect_of(epasses: Vec<kirie_scene::material::EffectPass>) -> kirie_scene::object::Effect {
        kirie_scene::object::Effect {
            file: "effects/test/effect.json".into(),
            id: 1,
            name: "test".into(),
            visible: UserSetting::literal(true),
            passes: vec![],
            resolved: Some(kirie_scene::material::EffectFile {
                name: String::new(),
                description: String::new(),
                group: String::new(),
                preview: String::new(),
                dependencies: vec![],
                fbos: vec![],
                passes: epasses,
            }),
        }
    }

    fn material_epass(shader: &str, target: Option<&str>) -> kirie_scene::material::EffectPass {
        kirie_scene::material::EffectPass {
            material: Some(format!("materials/effects/{shader}.json")),
            resolved: Some(Material {
                passes: vec![pass(shader, Blending::Normal)],
            }),
            bind: vec![],
            command: None,
            source: None,
            target: target.map(str::to_owned),
        }
    }

    fn command_epass(
        command: kirie_scene::material::PassCommand,
        source: &str,
        target: &str,
    ) -> kirie_scene::material::EffectPass {
        kirie_scene::material::EffectPass {
            material: None,
            resolved: None,
            bind: vec![],
            command: Some(command),
            source: Some(source.to_owned()),
            target: Some(target.to_owned()),
        }
    }

    #[test]
    fn copy_command_plans_the_virtual_blit_pass() {
        let mut img = image(vec![pass("base", Blending::Translucent)]);
        img.effects = vec![effect_of(vec![
            material_epass("motionblur_accumulation", Some("_rt_FullCompoBuffer2")),
            command_epass(PassCommand::Copy, "_rt_FullCompoBuffer2", "_rt_FullCompoBuffer1"),
            material_epass("motionblur_combine", None),
        ])];
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 4, "base + accumulate + copy + combine");
        let copy = &plan.passes[2];
        assert_eq!(copy.shader, COPY_COMMAND_SHADER);
        assert_eq!(
            copy.pass.textures,
            vec![Some("_rt_FullCompoBuffer2".to_owned())],
            "slot 0 must be the copy source (`CImage.cpp:711`)"
        );
        assert_eq!(copy.target.as_deref(), Some("_rt_FullCompoBuffer1"));
        assert_eq!(copy.blending, Blending::Normal);
        assert!(copy.binds.is_empty());
    }

    #[test]
    fn swap_command_pass_is_rejected_like_the_reference() {
        let mut img = image(vec![pass("base", Blending::Normal)]);
        img.effects = vec![effect_of(vec![command_epass(
            PassCommand::Swap,
            "_rt_SmokeDye1",
            "_rt_SmokeDye2",
        )])];
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 1, "swap contributes no pass");
    }

    #[test]
    fn command_passes_do_not_consume_pass_overrides() {
        use kirie_scene::object::PassOverride;
        let mut img = image(vec![pass("base", Blending::Normal)]);
        let mut effect = effect_of(vec![
            material_epass("accumulate", Some("_rt_FullCompoBuffer2")),
            command_epass(PassCommand::Copy, "_rt_FullCompoBuffer2", "_rt_FullCompoBuffer1"),
            material_epass("combine", None),
        ]);
        let ov = |v: i64| PassOverride {
            id: -1,
            combos: [("MARK".to_owned(), v)].into_iter().collect(),
            constantshadervalues: Default::default(),
            textures: vec![],
            usertextures: vec![],
        };
        effect.passes = vec![ov(1), ov(2)];
        img.effects = vec![effect];
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 4);
        assert_eq!(plan.passes[1].pass.combos.get("MARK"), Some(&1));
        assert_eq!(
            plan.passes[3].pass.combos.get("MARK"),
            Some(&2),
            "the combine pass takes the second override even though the copy \
             command sits between the material passes"
        );
        assert!(plan.passes[2].pass.combos.is_empty(), "copy takes no override");
    }

    fn effectpassthrough() -> Material {
        Material {
            passes: vec![pass("genericimage3", Blending::Normal)],
        }
    }

    #[test]
    fn color_blend_mode_appends_the_passthrough_pass() {
        let mut img = image(vec![pass("base", Blending::Additive)]);
        img.color_blend_mode = UserSetting::literal(9);
        let plan = plan_image(&img, true, false, Some(&effectpassthrough()));
        assert_eq!(plan.passes.len(), 2, "base copy + colorBlendMode pass");
        let last = &plan.passes[1];
        assert_eq!(last.shader, "genericimage3");
        assert_eq!(last.pass.combos.get("BLENDMODE"), Some(&9));
        assert_eq!(last.target, None, "renders into the composite chain");
        assert!(last.binds.is_empty());
        assert_eq!(last.output, PassOutput::Scene);
        assert_eq!(plan.passes[0].blending, Blending::Normal);
        assert_eq!(last.blending, Blending::Additive);
    }

    #[test]
    fn color_blend_mode_pass_follows_effect_passes() {
        let mut img = image(vec![pass("base", Blending::Normal)]);
        img.color_blend_mode = UserSetting::literal(2);
        img.effects = vec![effect_of(vec![material_epass("tint", None)])];
        let plan = plan_image(&img, true, false, Some(&effectpassthrough()));
        assert_eq!(plan.passes.len(), 3);
        assert_eq!(plan.passes[1].shader, "tint");
        assert_eq!(plan.passes[2].shader, "genericimage3");
        assert_eq!(plan.passes[2].pass.combos.get("BLENDMODE"), Some(&2));
    }

    #[test]
    fn default_color_blend_mode_appends_nothing() {
        let img = image(vec![pass("base", Blending::Normal)]);
        let plan = plan_image(&img, true, false, Some(&effectpassthrough()));
        assert_eq!(plan.passes.len(), 1);
    }

    #[test]
    fn color_blend_mode_without_builtin_material_degrades() {
        let mut img = image(vec![pass("base", Blending::Normal)]);
        img.color_blend_mode = UserSetting::literal(4);
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes.len(), 1);
    }

    #[test]
    fn blend_relocation_moves_first_to_last() {
        let img = image(vec![
            pass("a", Blending::Additive),
            pass("b", Blending::Translucent),
        ]);
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes[0].blending, Blending::Normal, "first forced Normal");
        assert_eq!(
            plan.passes[1].blending,
            Blending::Additive,
            "first's blend relocated to last"
        );
    }

    #[test]
    fn blend_relocation_is_unconditional_for_puppet_images() {
        let mut img = image(vec![
            pass("base", Blending::Translucent),
            pass("effect", Blending::Normal),
        ]);
        img.model = Some(kirie_scene::material::ModelFile {
            material: "materials/女.json".into(),
            solidlayer: false,
            fullscreen: false,
            passthrough: false,
            autosize: true,
            nopadding: false,
            width: None,
            height: None,
            puppet: Some("models/女_puppet.mdl".into()),
        });
        let plan = plan_image(&img, true, false, None);
        assert_eq!(plan.passes[0].blending, Blending::Normal, "no puppet exception");
        assert_eq!(
            plan.passes[1].blending,
            Blending::Translucent,
            "relocated to last"
        );
    }

    #[test]
    fn donor_blend_is_not_installed_on_last_pass() {
        let img = image(vec![
            pass("base", Blending::Translucent),
            pass("scroll", Blending::Normal),
            pass("transform", Blending::Normal),
        ]);
        let plan = plan_image(&img, true, true, None);
        assert_eq!(plan.passes[0].blending, Blending::Normal, "copy is replace");
        assert_eq!(
            plan.passes[2].blending,
            Blending::Normal,
            "layer blending suppressed: the composite stays straight-alpha"
        );
    }

    #[test]
    fn single_pass_donor_copy_is_replace() {
        let img = image(vec![pass("base", Blending::Translucent)]);
        let plan = plan_image(&img, true, true, None);
        assert_eq!(plan.passes[0].blending, Blending::Normal);
    }
}
