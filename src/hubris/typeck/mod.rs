mod error;
mod inductive;
mod name_generator;

use core::*;
use super::ast::{SourceMap, Span, HasSpan};
use super::parser;
use super::elaborate::{self, Error as ElabErr};

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{PathBuf, Path};

use self::name_generator::*;
pub use self::error::Error;
use error_reporting::{ErrorContext, Report};
use term::{Terminal, stdout, color, StdoutTerminal, Result as TResult};

/// A global context for type checking containing the necessary information
/// needed across type checking all definitions.
pub struct TyCtxt {
    // We keep these around right now, but I'm not sure if we should.
    types: HashMap<Name, Data>,
    functions: HashMap<Name, Function>,

    axioms: HashMap<Name, Term>,
    definitions: HashMap<Name, (Term, Term)>,

    pub source_map: SourceMap,

    local_counter: RefCell<usize>,
    pub terminal: Box<StdoutTerminal>,
}

impl ErrorContext<io::Stdout> for TyCtxt {
    fn get_source_map(&self) -> &SourceMap {
        &self.source_map
    }

    fn get_terminal(&mut self) -> &mut Box<Terminal<Output=io::Stdout> + Send> {
        &mut self.terminal
    }
}

impl TyCtxt {
    pub fn empty() -> TyCtxt {
        TyCtxt {
            types: HashMap::new(),
            functions: HashMap::new(),
            axioms: HashMap::new(),
            definitions: HashMap::new(),
            source_map: SourceMap::from_file("".to_string(), "".to_string()),
            local_counter: RefCell::new(0),
            terminal: stdout().unwrap(),
        }
    }

    pub fn from_module(module: &Module, source_map: SourceMap) -> Result<TyCtxt, Error> {
        let mut ty_cx = TyCtxt::empty();
        ty_cx.source_map = source_map;

        try!(ty_cx.type_check_module(module));

        Ok(ty_cx)
    }

    pub fn type_check_module(&mut self, module: &Module) -> Result<(), Error> {
        let main_file = PathBuf::from(self.source_map.file_name.clone());
        // let prefix = main_file.parent().unwrap();

        // Should be idempotent, is currently not.
        // for import in &module.imports {
        //     try!(self.load_import(&prefix, import));
        // }

        for def in &module.defs {
            match def {
                &Item::Data(ref d) => try!(self.declare_datatype(d)),
                &Item::Fn(ref f) => self.declare_def(f),
                &Item::Extern(ref e) => self.declare_extern(e),
            }

            try!(self.type_check_def(def));
        }


        Ok(())
    }

    pub fn in_scope(&self, name: &Name) -> bool {
        self.axioms.contains_key(name) || self.definitions.contains_key(name)
    }

    pub fn load_import(&mut self, path: &Path, name: &Name) -> Result<(), Error> {
        debug!("load_import: path={} module={}", path.display(), name);
        let file_suffix = match name_to_path(name) {
            None => panic!(),
            Some(f) => f,
        };

        let file_to_load = path.join(file_suffix);
        debug!("load_import: file_to_load={}", file_to_load.display());

        let parser = try!(parser::from_file(&file_to_load));
        let module = try!(parser.parse());
        let mut ecx = elaborate::ElabCx::from_module(module, parser.source_map.clone());

        let emodule = ecx.elaborate_module(&file_to_load);

        // Should find a way to gracefully exit, or report error and continue function
        match emodule {
            Err(e) => {
                e.report(&mut ecx);
                // We should return an import error here
                Ok(())
            },
            Ok(emodule) => {
                let ty_cx = try!(TyCtxt::from_module(&emodule, self.source_map.clone()));
                self.merge(ty_cx)
            }
        }
    }

    pub fn merge(&mut self, ty_cx: TyCtxt) -> Result<(), Error> {
        let TyCtxt {
            types,
            functions,
            axioms,
            definitions,
            ..
        } = ty_cx;

        let mut errors = vec![];

        for (n, ty) in types {
            if let Some(_) = self.types.insert(n.clone(), ty) {
                errors.push(Error::NameExists(n))
            }
        }

        for (n, fun) in functions {
            if let Some(_) = self.functions.insert(n.clone(), fun) {
                errors.push(Error::NameExists(n))
            }
        }

        for (n, axiom) in axioms {
            if let Some(_) = self.axioms.insert(n.clone(), axiom) {
                errors.push(Error::NameExists(n))
            }

        }

        for (n, def) in definitions {
            if let Some(_) = self.definitions.insert(n.clone(), def) {
                errors.push(Error::NameExists(n));
            }
        }

        if errors.len() != 0 {
            Err(Error::Many(errors))
        } else {
            Ok(())
        }
    }

    pub fn get_main_body(&self) -> Result<&Term, Error> {
        match self.functions.get(&Name::from_str("main")) {
            None => Err(Error::NoMain),
            Some(ref f) => Ok(&f.body),
        }
    }

    pub fn declare_datatype(&mut self, data_type: &Data) -> Result<(), Error> {
        // Currently we use types/functions for metadata, do we need them?
        self.types.insert(data_type.name.clone(), data_type.clone());

        // The type is just a constant with the type `ty`
        self.axioms.insert(data_type.name.clone(), data_type.ty.clone());

        // Each constructor also becomes a constant with type ascribed
        // in its definition.
        for ctor in &data_type.ctors {
            let name = ctor.0.clone();
            let ty = ctor.1.clone();
            self.axioms.insert(name, ty);
        }

        inductive::make_recursor(self, data_type)
    }

    pub fn declare_def(&mut self, f: &Function) {
        self.functions.insert(f.name.clone(), f.clone());
        self.definitions.insert(f.name.clone(), (f.ret_ty.clone(), f.body.clone()));
    }

    /// Declaring an external function creates an axiom in the type checker
    /// with the appropriate type.
    ///
    /// During code generation we will deal with creating a symbol for this
    /// function.
    pub fn declare_extern(&mut self, e: &Extern) {
        self.axioms.insert(e.name.clone(), e.term.clone());
    }

    pub fn type_check_def(&self, def: &Item) -> Result<(), Error> {
        debug!("type_check_def: def={}", def);
        match def {
            &Item::Fn(ref fun) => {
                let &Function {
                    ref ret_ty,
                    ref body, ..
                } = fun;

                let mut lcx = LocalCx::from_cx(self);
                try!(lcx.type_check_term(&body, &ret_ty));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn lookup_global(&self, name: &Name) -> Result<&Term, Error> {
        match self.definitions.get(name) {
            None => {
                match self.axioms.get(name) {
                    None => Err(Error::UnknownVariable(name.clone())),
                    Some(t) => Ok(t),
                }
            }
            Some(t) => Ok(&t.0),
        }
    }

    pub fn local(&self, name: &Name, ty: Term) -> Name {
        let repr = match name {
            &Name::DeBruijn { ref repr, .. } => repr,
            _ => panic!("creating local {:?}", name),
        };

        self.local_with_repr(repr.clone(), ty)
    }

    pub fn local_with_repr(&self, repr: String, ty: Term) -> Name {
        let new_local = Name::Local {
            number: *self.local_counter.borrow(),
            ty: Box::new(ty),
            repr: repr.clone(),
        };

        *self.local_counter.borrow_mut() += 1;

        new_local
    }

    /// Will try to unfold a name if it is unfoldable
    pub fn unfold_name(&self, n: &Name) -> Result<Term, Error> {
        use core::Name::*;

        match n {
            q @ &Qual { .. } => {
                // TODO: also check axioms and report an error about unfolding axioms
                // TODO: we actually need to know whether a name is Opaque or not
                // Or we can't implement this
                match self.definitions.get(q) {
                    None => Ok(n.to_term()), // panic!("failed to lookup name {}", q),
                    Some(t) => Ok(t.1.clone()),
                }
            }
            &DeBruijn { .. } |
            &Meta { .. } |
            &Local { .. } => Ok(n.to_term()),
        }
    }

    pub fn unfold(&self, mut t: Term, n: &Name) -> Result<Term, Error> {
        let def_rhs = try!(self.unfold_name(n));
        let nt = n.to_term();

        t.replace_term(&def_rhs,
                       &|term| self.def_eq(Span::dummy(), term, &nt).is_err());

        Ok(t)
    }

    pub fn eval(&self, term: &Term) -> Result<Term, Error> {
        use core::Term::*;

        debug!("eval: {}", term);

        let result = match term {
            &App { ref fun, ref arg, span } => {
                let efun = try!(self.eval(fun));
                // This is call by value
                let earg = try!(self.eval(arg));

                match efun {
                    Term::Lambda { ref body, .. } => {
                        self.eval(&body.instantiate(&earg))
                    }
                    f => Ok(App {
                        fun: Box::new(f),
                        arg: Box::new(earg),
                        span: span,
                    })
                }
            }
            &Term::Forall { ref name, ref ty, ref term, span } => {
                let ety = try!(self.eval(ty));
                let eterm = try!(self.eval(term));

                Ok(Forall {
                    name: name.clone(),
                    ty: Box::new(ety),
                    term: Box::new(eterm),
                    span: span,
                })
            }
            &Term::Var { ref name } => self.unfold_name(name),
            &Term::Recursor(ref ty_name, offset, ref ts) => {
                // for t in ts {
                    // println!("ARG: {}", t);
                // }
                match self.types.get(&ty_name) {
                    None => panic!("can not find decl for {}", ty_name),
                    Some(dt) => {
                        let scrutinee = try!(self.eval(&ts[ts.len() - 1]));
                        // Super hack-y right now, need to account for
                        // the type formers, probably should just
                        // store an offset into the vector of
                        // terms to keep this model simple.
                        //
                        // We need to have all the binding structure
                        // of the type in order of the substitions
                        // to correctly work.
                        for (i, ctor) in dt.ctors.iter().enumerate() {
                            let name = &ctor.0;
                            debug!("name of ctor: {}", name);
                            debug!("arg to recursor: {}", scrutinee);
                            match scrutinee.head() {
                                None => panic!("arg to recursor must be in (w)hnf"),
                                Some(head) => {
                                    if name.to_term() == head {
                                        let premise = ts[i + offset].clone();
                                        // I think instead we need to figure out if
                                        // this is recursive contructor case.
                                        match scrutinee.args() {
                                            None => return Ok(premise),
                                            Some(mut args) => {
                                                let mut tsprime = ts.clone();
                                                let idx = tsprime.len() - 1;
                                                tsprime[idx] = args[0].clone();
                                                let rec = Recursor(ty_name.clone(),
                                                                   offset,
                                                                   tsprime);
                                                args.push(rec);
                                                return self.eval(&Term::apply_all(premise, args));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        panic!("this shouldn't happen")
                    }
                }
            }
            t => Ok(t.clone()),
        };

        debug!("eval result {:?}", result);

        result
    }

    pub fn def_eq(&self, span: Span, t: &Term, u: &Term) -> Result<Term, Error> {
        debug!("unify: {} {}", t, u);
        let t = try!(self.eval(t));
        let u = try!(self.eval(u));

        let mut inequalities = vec![];
        let is_def_eq = def_eq_modulo(&t, &u, &mut inequalities);
        if is_def_eq {
            assert_eq!(inequalities.len(), 0);
            Ok(t.clone())
        } else {
            Err(Error::DefUnequal(span, t.clone(), u.clone(), inequalities))
        }
    }
}

#[derive(Clone)]
pub struct LocalCx<'tcx> {
    ty_cx: &'tcx TyCtxt,
    // Local entries in the typing context.
    locals: HashMap<Name, Term>,
    // I think this should be more flexible
    equalities: HashMap<Term, Term>,
}

impl<'tcx> LocalCx<'tcx> {
    pub fn from_cx(ty_cx: &'tcx TyCtxt) -> LocalCx<'tcx> {
        LocalCx {
            ty_cx: ty_cx,
            locals: HashMap::new(),
            equalities: HashMap::new(),
        }
    }

    #[inline]
    pub fn local(&mut self, name: &Name, ty: Term) -> Name {
        self.ty_cx.local(name, ty)
    }


    pub fn type_check_term(&mut self, term: &Term, ty: &Term) -> Result<Term, Error> {
        debug!("type_check_term: infering the type of {}", term);
        let infer_ty = try!(self.type_infer_term(term));
        debug!("type_check_term: checking {} againist the inferred type {}",
               ty,
               infer_ty);
        let term = try!(self.ty_cx.def_eq(term.get_span(), ty, &infer_ty));
        Ok(term)
    }

    pub fn type_infer_term(&mut self, term: &Term) -> Result<Term, Error> {
        match term {
            &Term::Literal { ref lit, .. } => {
                match lit {
                    &Literal::Int(..) => Ok(panic!()),
                    &Literal::Unit => Ok(Term::Var { name: Name::from_str("Unit") }),
                }
            }
            &Term::Var { ref name, .. } => {
                match name {
                    &Name::Local { ref ty, .. } => Ok(*ty.clone()),
                    q @ &Name::Qual { .. } => self.ty_cx.lookup_global(q).map(Clone::clone),
                    _ => {
                        panic!("internal error: all variable occurences must be free when type \
                                checking
                            term that is a variable")
                    }
                }
            }
            &Term::App { ref fun, ref arg, span } => {
                match try!(self.type_infer_term(fun)) {
                    Term::Forall { term, ty, .. } => {
                        // When doing inference I don't think we should try to check this
                        // constraint:
                        try!(self.type_check_term(arg, &*ty));
                        Ok(term.instantiate(arg))
                    }
                    _ => Err(Error::ApplicationMismatch(
                        span,
                        *fun.clone(),
                        *arg.clone())),
                }
            }
            &Term::Forall { ref name, ref ty, ref term, .. } => {
                let local = self.local(name, *ty.clone());
                let term = term.instantiate(&local.to_term());

                try!(self.type_check_term(&*ty, &Term::Type));
                try!(self.type_check_term(&term, &Term::Type));

                Ok(Term::Type)
            }
            &Term::Lambda { ref name, ref ty, ref body, span, } => {
                let local = self.local(name, *ty.clone());

                let body = body.instantiate(&local.to_term());

                let pi_body = try!(self.type_infer_term(&body)).abstr(&local);

                Ok(Term::Forall {
                    span: span,
                    name: name.clone(),
                    ty: ty.clone(),
                    term: Box::new(pi_body),
                })
            }
            &Term::Type => Ok(Term::Type),
            _ => panic!(),
        }
    }

    pub fn evaluate(&self, term: &Term) -> Term {
        term.clone()
    }
}

fn def_eq_modulo(t1: &Term, t2: &Term, equalities: &mut Vec<(Term, Term)>) -> bool {
    use core::Term::*;

    debug!("equal_modulo: {} == {}", t1, t2);

    match (t1, t2) {
        (&App { fun: ref fun1, arg: ref arg1, .. },
         &App { fun: ref fun2, arg: ref arg2, .. }) => {
            def_eq_modulo(fun1, fun2, equalities) && def_eq_modulo(arg1, arg2, equalities)
        }
        (&Forall { ty: ref ty1, term: ref term1, .. },
         &Forall { ty: ref ty2, term: ref term2, .. }) => {
            def_eq_modulo(ty1, ty2, equalities) && def_eq_modulo(term1, term2, equalities)
        }
        (&Lambda { ty: ref ty1, body: ref body1, .. },
         &Lambda { ty: ref ty2, body: ref body2, ..}) => {
            def_eq_modulo(ty1, ty2, equalities) && def_eq_modulo(body1, body2, equalities)
        }
        (t, u) => {
            if t == u {
                true
            } else {
                equalities.push((t.clone(), u.clone()));
                false
            }
        }
    }
}

fn name_to_path(name: &Name) -> Option<PathBuf> {
    match name {
        &Name::Qual { ref components, .. } => {
            assert!(components.len() > 0);
            let mut cs = components.iter();
            let first = cs.next().unwrap();
            let mut path = PathBuf::from(first);

            for c in cs {
                path = path.join(c);
            }

            path.set_extension("hb");

            Some(path)
        }
        _ => None,
    }
}
