use ast::*;
use config::Config;
use failure::ResultExt;
use library::Lib;
use melon::{IntegerType, Register, typedef::*};
use parser::{BeastParser, Rule};
use pest::{Parser, iterators::Pair};
use std::{
    thread, collections::{BTreeMap, BTreeSet}, fs::File, io::Read, path::PathBuf,
    sync::mpsc::{self, TryRecvError},
};

const BEAST_SOURCE_FILE_EXTENSIONS: [&str; 2] = ["beast", "bst"];
const BEAST_LIB_FILE_EXTENSIONS: [&str; 2] = ["blib", "bl"];
const BEAST_DEFAULT_LIB_PATH: &str = "lib";
const BEAST_DEFAULT_INCLUDE_PATH: &str = "src";
pub const BEAST_DEFAULT_ENTRY_POINT_MODULE: &str = "main";
pub const BEAST_ENTRY_POINT_FUNC: &str = "$main";

#[derive(Clone)]
pub struct AstGen {
    config: Config,
    lib: Vec<String>,
    include: Vec<String>,
}

impl AstGen {
    fn new(config: Config) -> AstGen {
        let compilation = config.compilation.clone().unwrap_or_default();

        let lib = compilation
            .lib
            .clone()
            .unwrap_or(vec![BEAST_DEFAULT_LIB_PATH.into()]);

        let include = compilation
            .include
            .clone()
            .unwrap_or(vec![BEAST_DEFAULT_INCLUDE_PATH.into()]);

        AstGen {
            config: config,
            lib: lib,
            include: include,
        }
    }

    pub fn gen(root_module: String, config: Config) -> Result<Ast> {
        let mut compiler = AstGen::new(config);
        let ast = compiler.ast(root_module)?;

        Ok(ast)
    }

    fn ast(&mut self, root_module: String) -> Result<Ast> {
        let (module_sender, module_receiver) = mpsc::channel();
        let (instructor_sender, instructor_receiver) = mpsc::channel::<String>();

        instructor_sender.send(root_module.clone())?;

        let compiler = self.clone();
        let instructor_sender = instructor_sender.clone();
        thread::spawn(move || {
            while let Ok(module_name) = instructor_receiver.recv() {
                let mut compiler = compiler.clone();
                let module_sender = module_sender.clone();
                thread::spawn(move || {
                    let module = compiler.module(module_name.clone());

                    module_sender.send((module_name, module)).unwrap();
                });
            }
        });

        let mut modules = BTreeMap::new();
        let mut requested_modules = BTreeSet::new();
        requested_modules.insert(root_module);

        loop {
            match module_receiver.try_recv() {
                Ok((module_name, module_res)) => {
                    let module = module_res.with_context(|e| {
                        format!("failed to compile module {:?}\n{}", module_name, e)
                    })?;

                    modules.insert(module_name, module.clone());

                    if let Module::Source { ref imports, .. } = module {
                        let imports = imports.clone();
                        for import in imports {
                            if !requested_modules.contains(&import.module_path) {
                                requested_modules.insert(import.module_path.clone());

                                instructor_sender.send(import.module_path)?;
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    if modules.len() == requested_modules.len() {
                        break;
                    }

                    thread::yield_now();
                }
                _ => bail!("an unknown error occured"),
            }
        }

        Ok(Ast { modules: modules })
    }

    fn module(&mut self, module_path: String) -> Result<Module> {
        let module = self.discover_module(module_path.clone())?;

        if let ModuleSource::Lib(lib) = module {
            return Ok(Module::Lib(lib));
        }

        let module_file = if let ModuleSource::Module(module_file) = module {
            module_file
        } else {
            unreachable!()
        };

        let mut file = File::open(module_file)?;

        let mut buf = String::new();

        file.read_to_string(&mut buf)?;

        let parsing_result = BeastParser::parse(Rule::file, &buf);

        if let Err(err) = parsing_result {
            bail!("{}", err);
        }

        let parsed_file = parsing_result.unwrap();

        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut constants = Vec::new();
        let mut funcs = Vec::new();

        for pair in parsed_file {
            match pair.as_rule() {
                Rule::import => {
                    let import = self.import(pair)?;
                    imports.push(import);
                }
                Rule::func => {
                    let func = self.func(pair)?;
                    funcs.push(func);
                }
                Rule::export => {
                    let export = self.export(pair)?;
                    exports.push(export);
                }
                Rule::constant => {
                    let constant = self.constant(pair)?;
                    constants.push(constant);
                }
                _ => unreachable!(),
            }
        }

        Ok(Module::Source {
            path: module_path,
            imports,
            exports,
            constants,
            funcs,
        })
    }

    fn import(&mut self, pair: Pair<Rule>) -> Result<Import> {
        let mut pairs = pair.into_inner();

        let func_name = pairs.next().unwrap().as_str();

        let after_func = pairs.next().unwrap();

        let (func_alias, module_path) = if after_func.as_rule() == Rule::func_alias {
            (Some(after_func.as_str()), pairs.next().unwrap().as_str())
        } else {
            (None, after_func.as_str())
        };

        let mut module_path = module_path.to_owned();
        module_path.pop();
        module_path.remove(0);

        Ok(Import {
            origin_name: func_name.into(),
            alias: func_alias.unwrap_or(func_name).into(),
            module_path: module_path,
        })
    }

    fn func(&mut self, pair: Pair<Rule>) -> Result<Func> {
        let mut pairs = pair.into_inner();

        let func_name = pairs.next().unwrap().as_str();

        let mut instr_vec = Vec::new();

        for instr in pairs {
            let instr = self.instr(instr)?;

            instr_vec.push(instr);
        }

        Ok(Func {
            name: func_name.into(),
            instr: instr_vec,
        })
    }

    fn constant(&mut self, pair: Pair<Rule>) -> Result<Const> {
        let mut pairs = pair.into_inner();

        let const_name = pairs.next().unwrap().as_str();

        let raw_const_lit = pairs.next().unwrap().as_str();

        Ok(Const {
            name: const_name.into(),
            value: raw_const_lit.parse()?,
        })
    }

    fn export(&mut self, pair: Pair<Rule>) -> Result<Export> {
        let mut pairs = pair.into_inner();

        let exported_func = pairs.next().unwrap().as_str();

        let alias = pairs.next().and_then(|e| Some(e.as_str())).or_else(|| None);

        Ok(Export {
            origin_name: exported_func.into(),
            alias: alias.unwrap_or(exported_func).into(),
        })
    }

    fn instr(&mut self, pair: Pair<Rule>) -> Result<Instruction> {
        let mut pairs = pair.into_inner();

        let plain_instr = pairs.next().unwrap();
        let mut inner = plain_instr.clone().into_inner();

        match plain_instr.as_rule() {
            Rule::push_instr => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);

                let raw_arg = inner.next().unwrap();
                match raw_arg.as_rule() {
                    Rule::constant_id => {
                        let arg = raw_arg.as_str();
                        let inst = match real_type {
                            IntegerType::U8 => {
                                Instruction::PushConstU8(Argument::Constant(arg.into()))
                            }
                            IntegerType::U16 => {
                                Instruction::PushConstU16(Argument::Constant(arg.into()))
                            }
                            IntegerType::I8 => {
                                Instruction::PushConstI8(Argument::Constant(arg.into()))
                            }
                            IntegerType::I16 => {
                                Instruction::PushConstI16(Argument::Constant(arg.into()))
                            }
                        };

                        Ok(inst)
                    }
                    Rule::literal => {
                        let arg = raw_arg.as_str();
                        let inst = match real_type {
                            IntegerType::U8 => Instruction::PushConstU8(Argument::Literal(
                                arg.parse().or_else(|_| u8::from_str_radix(&arg[2..], 16))?,
                            )),
                            IntegerType::U16 => Instruction::PushConstU16(Argument::Literal(
                                arg.parse().or_else(|_| u16::from_str_radix(&arg[2..], 16))?,
                            )),
                            IntegerType::I8 => Instruction::PushConstI8(Argument::Literal(
                                arg.parse().or_else(|_| i8::from_str_radix(&arg[2..], 16))?,
                            )),
                            IntegerType::I16 => Instruction::PushConstI16(Argument::Literal(
                                arg.parse().or_else(|_| i16::from_str_radix(&arg[2..], 16))?,
                            )),
                        };

                        Ok(inst)
                    }
                    _ => unreachable!(),
                }
            }
            Rule::add => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Add(real_type))
            }
            Rule::sub => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Sub(real_type))
            }
            Rule::mul => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Mul(real_type))
            }
            Rule::div => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Div(real_type))
            }
            Rule::shr => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Shr(real_type))
            }
            Rule::shl => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Shl(real_type))
            }
            Rule::and => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::And(real_type))
            }
            Rule::or => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Or(real_type))
            }
            Rule::xor => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Xor(real_type))
            }
            Rule::not => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Not(real_type))
            }
            Rule::neg => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Neg(real_type))
            }
            Rule::inc => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Inc(real_type))
            }
            Rule::dec => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Dec(real_type))
            }
            Rule::u8_promote => Ok(Instruction::U8Promote),
            Rule::u16_demote => Ok(Instruction::U16Demote),
            Rule::i8_promote => Ok(Instruction::I8Promote),
            Rule::i16_demote => Ok(Instruction::I16Demote),
            Rule::reg => {
                let raw_register = inner.next().unwrap().as_str();

                Ok(Instruction::LoadReg(self.register(raw_register)?))
            }
            Rule::load => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);

                if let Some(raw_arg) = inner.next() {
                    let arg = if raw_arg.as_rule() == Rule::constant_id {
                        Argument::Constant(raw_arg.as_str().into())
                    } else {
                        let raw_arg = raw_arg.as_str();
                        Argument::Literal(raw_arg
                            .parse()
                            .or_else(|_| u16::from_str_radix(&raw_arg[2..], 16))?)
                    };

                    Ok(Instruction::Load(real_type, arg))
                } else {
                    Ok(Instruction::LoadIndirect(real_type))
                }
            }
            Rule::store => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);

                if let Some(raw_arg) = inner.next() {
                    let arg = if raw_arg.as_rule() == Rule::constant_id {
                        Argument::Constant(raw_arg.as_str().into())
                    } else {
                        let raw_arg = raw_arg.as_str();
                        Argument::Literal(raw_arg
                            .parse()
                            .or_else(|_| u16::from_str_radix(&raw_arg[2..], 16))?)
                    };

                    Ok(Instruction::Store(real_type, arg))
                } else {
                    Ok(Instruction::StoreIndirect(real_type))
                }
            }
            Rule::dup => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Dup(real_type))
            }
            Rule::drop => {
                let raw_type = inner.next().unwrap().as_str();
                let real_type = self.type_(raw_type);
                Ok(Instruction::Drop(real_type))
            }
            Rule::sys => {
                let signal = inner.next().unwrap().as_str();
                Ok(Instruction::Sys(signal.into()))
            }
            Rule::call => {
                let func_id = inner.next().unwrap().as_str();
                Ok(Instruction::Call(func_id.into()))
            }
            Rule::ret => Ok(Instruction::Ret),
            Rule::alloc => {
                let raw_num_const = inner.next().unwrap();

                let arg = if raw_num_const.as_rule() == Rule::constant_id {
                    Argument::Constant(raw_num_const.as_str().into())
                } else {
                    let raw_arg = raw_num_const.as_str();

                    Argument::Literal(raw_arg
                        .parse()
                        .or_else(|_| u16::from_str_radix(&raw_arg[2..], 16))?)
                };

                Ok(Instruction::Alloc(arg))
            }
            Rule::free => Ok(Instruction::Free),
            Rule::while_loop => {
                let cond = inner.next().unwrap();

                let condition = match cond.as_rule() {
                    Rule::greater => IfCond::Positive,
                    Rule::less => IfCond::Negative,
                    Rule::equal => IfCond::Zero,
                    Rule::unequal => IfCond::NotZero,
                    _ => unreachable!(),
                };

                let type_t = inner.next().unwrap().as_str();
                let real_type = self.type_(type_t);

                let mut instr_vec = Vec::new();

                for instr in inner {
                    let instr = self.instr(instr)?;

                    instr_vec.push(instr);
                }

                Ok(Instruction::While(While(condition, real_type, instr_vec)))
            }
            Rule::if_cond => {
                let cond = inner.next().unwrap();

                let condition = match cond.as_rule() {
                    Rule::greater => IfCond::Positive,
                    Rule::less => IfCond::Negative,
                    Rule::equal => IfCond::Zero,
                    Rule::unequal => IfCond::NotZero,
                    _ => unreachable!(),
                };

                let type_t = inner.next().unwrap().as_str();
                let real_type = self.type_(type_t);

                let mut instr_vec = Vec::new();

                let mut else_branch = None;

                for instr in inner {
                    if instr.as_rule() == Rule::else_cond {
                        let mut else_instr_vec = Vec::new();

                        for instr in instr.into_inner() {
                            let instr = self.instr(instr)?;

                            else_instr_vec.push(instr);
                        }

                        else_branch = Some(else_instr_vec);
                        break;
                    }

                    let instr = self.instr(instr)?;

                    instr_vec.push(instr);
                }

                Ok(Instruction::If(If(
                    condition,
                    real_type,
                    instr_vec,
                    else_branch,
                )))
            }
            _ => unreachable!(),
        }
    }

    fn type_(&mut self, raw: &str) -> IntegerType {
        match raw {
            "u8" => IntegerType::U8,
            "u16" => IntegerType::U16,
            "i8" => IntegerType::I8,
            "i16" => IntegerType::I16,
            _ => unreachable!(),
        }
    }

    fn register(&mut self, raw: &str) -> Result<Register> {
        let res = match raw {
            ":sp" => Register::StackPtr,
            ":bp" => Register::BasePtr,
            reg => bail!(
                "unrecognized register identifier: {:?} is not one of {:?}",
                reg,
                vec![":sp", ":bp"]
            ),
        };

        Ok(res)
    }

    fn discover_module(&mut self, module: String) -> Result<ModuleSource> {
        let orig_module = module.clone();

        let blib_module_name =
            PathBuf::from(&orig_module).with_extension(BEAST_LIB_FILE_EXTENSIONS[0]);

        let bl_module_name =
            PathBuf::from(&orig_module).with_extension(BEAST_LIB_FILE_EXTENSIONS[1]);

        let beast_module_name =
            PathBuf::from(&orig_module).with_extension(BEAST_SOURCE_FILE_EXTENSIONS[0]);

        let bst_module_name =
            PathBuf::from(&orig_module).with_extension(BEAST_SOURCE_FILE_EXTENSIONS[1]);

        let found_lib = self.lib
            .iter()
            .map(|lib| PathBuf::from(lib).join(blib_module_name.clone()))
            .chain(
                self.lib
                    .iter()
                    .map(|lib| PathBuf::from(lib).join(bl_module_name.clone())),
            )
            .find(|lib| lib.exists());

        if let Some(lib_path) = found_lib {
            let lib = Lib::from_file(lib_path)?;
            return Ok(ModuleSource::Lib(lib));
        }

        let found_module = self.include
            .iter()
            .map(|include| PathBuf::from(include).join(beast_module_name.clone()))
            .chain(
                self.include
                    .iter()
                    .map(|include| PathBuf::from(include).join(bst_module_name.clone())),
            )
            .find(|include| include.exists());

        if let Some(module_path) = found_module {
            return Ok(ModuleSource::Module(module_path));
        }

        bail!("unable to find module: {:?}", module)
    }
}

enum ModuleSource {
    Module(PathBuf),
    Lib(Lib),
}