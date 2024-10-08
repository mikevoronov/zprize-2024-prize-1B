use ark_bls12_381::{Bls12_381, Fr, G1Affine};
use ark_ed_on_bls12_381::EdwardsParameters;
use ark_poly_commit::PolynomialCommitment;
use ark_std::rand::RngCore;
use ark_std::{test_rng, UniformRand};
use merkle_tree::HEIGHT;
use merkle_tree::{MerkleTree, MerkleTreeCircuit};
use plonk_core::commitment::KZG10;
use plonk_core::prelude::{
    verify_proof, Circuit, StandardComposer, VerifierData,
};
use plonk_core::proof_system::Prover;
use plonk_core::permutation::MSM_KERN;
use plonk_hashing::poseidon::constants::PoseidonConstants;
use plonk_hashing::poseidon::poseidon_ref::NativeSpecRef;
use plonk_core::constraint_system;
use crate::constraint_system::Variable;

use ec_gpu_common::api::{*};
use std::os::raw::c_void;
use std::os::raw::c_int;
use ec_gpu_common::G1AffineNoInfinity;

fn main() {
    let mut rng = test_rng();
    let param = PoseidonConstants::<Fr>::generate::<3>();
    
    let mut matrix : Vec<Fr> = vec![];
    let mut tag = Vec::<Fr>::new(); tag.resize(1, param.domain_tag); tag[0] = param.domain_tag; 
    
    for z in (0..3) {
        for k in (0..3) {
            matrix.push(param.mds_matrices.m.0[z][k]);
        }
    }
    unsafe { sync_hash_consts(param.full_rounds as u64, param.half_full_rounds as u64 , param.partial_rounds as u64, param.round_constants.len() as u64, 
                              param.round_constants.as_ptr() as *const c_void, tag.as_ptr() as *const c_void, matrix.as_ptr() as *const c_void); }

    // ==============================
    // first we build a merkle tree
    // ==============================

    let leaf_nodes = (0..1 << (HEIGHT - 1))
        .map(|_| Fr::rand(&mut rng))
        .collect::<Vec<_>>();
    let tree = MerkleTree::<NativeSpecRef<Fr>>::new_with_leaf_nodes(
        &param,
        &leaf_nodes,
    );

    
    let index = rng.next_u32() % (1 << (HEIGHT - 1));
    let proof = tree.gen_proof(index as usize);
    let _res = proof.verify(&param, &tree.root());

    // omitted: parameters too large
    // println!("generating merkle tree with parameter {:?}", param);

    //println!("merkle tree with height: {}:\n{}\n", HEIGHT, tree);
    //println!(
    //    "merkle proof for {}-th leaf: {}\n{}\n",
    //    index, leaf_nodes[index as usize], proof
    //);
    //println!("proof is valid: {}", res);

    // ==============================
    // next we generate the constraints for the tree
    // ==============================

    let mut composer = StandardComposer::<Fr, EdwardsParameters>::new();
    tree.gen_constraints(&mut composer, &param);

    composer.check_circuit_satisfied();

    // ==============================
    // last we generate the plonk proof
    // ==============================
    {
        // public parameters
        let size = 1 << (HEIGHT + 9);
        let pp = KZG10::<Bls12_381>::setup(size, None, &mut rng).unwrap();

        let mut dummy_circuit = MerkleTreeCircuit {
            param: param.clone(),
            merkle_tree: tree,
        };
        
        // preprocessing
        let (pk, (vk, _pi_pos)) = dummy_circuit.compile::<KZG10<Bls12_381>>(&pp).unwrap();

        // proof generation
        let leaf_nodes = (0..1 << (HEIGHT - 1)).map(|_| Fr::rand(&mut rng)).collect::<Vec<_>>();
        let tree = MerkleTree::<NativeSpecRef<Fr>>::new_with_leaf_nodes(&param,&leaf_nodes);
        let mut real_circuit = MerkleTreeCircuit { param: param.clone(),merkle_tree: tree};

        let coeffs_count = 1 << (HEIGHT + 7);
        let (ck, _) = <KZG10<Bls12_381>>::trim(&pp, real_circuit.padded_circuit_size(), 0, None).unwrap();

        //TODO: comment allocations!
        //let gpu_context = <KZG10<Bls12_381>>::get_gpu_context(&MSM_KERN, &ck, coeffs_count);
        
        let bases = ck.powers().powers_of_g.to_vec();
        let bases_no_infinity = bases.iter().map(G1AffineNoInfinity::from).collect::<Vec<_>>();
        unsafe {            
           init_bases_and_data(bases_no_infinity.as_ptr() as *const c_void, coeffs_count as i32);
           sync_mt(HEIGHT as i32, 
                   real_circuit.merkle_tree.non_leaf_nodes.as_ptr() as *const c_void, 
                   real_circuit.merkle_tree.leaf_nodes.as_ptr() as *const c_void);
        }
        let mut prover = Prover::<Fr, EdwardsParameters, KZG10<Bls12_381>>::new(b"Merkle tree");
        prover.prover_key = Some(pk.clone());
        prover.copy_data(coeffs_count as i32);
        let mut cs = prover.mut_cs();
        
        let hash_vars = cs.get_vars();
        let mut vars : Vec<Fr> = vec![];
        for i in (0..hash_vars.len()) {
            vars.push(hash_vars[&cs.get_varib(i as usize)]);
        }

        unsafe { sync_composer(HEIGHT as i32,
                               cs.get_wl().as_ptr() as *const c_void,
                               cs.get_wr().as_ptr() as *const c_void,
                               cs.get_wo().as_ptr() as *const c_void,
                               cs.get_w4().as_ptr() as *const c_void,
                               cs.get_wl().len() as u64, 
                               vars.as_ptr() as *const c_void, cs.get_vars().len() as u64); 
                //build_constraints();
        }

        //let now = std::time::Instant::now();
        //TODO: need to calculate circuit_bound using PoseidonConstants and removed this call
        real_circuit.gadget(cs);        
        //println!("constraints time is {:?}", now.elapsed());
                
        /*let hash_vars = cs.get_vars();
        let mut vars : Vec<Fr> = vec![];
        for i in (0..hash_vars.len()) {
            vars.push(hash_vars[&cs.get_varib(i as usize)]);
        }
        let (w_l_scalar, w_r_scalar, w_o_scalar, w_4_scalar) = prover.to_scalars4();
        
        unsafe { check_constraints(vars.as_ptr() as *const c_void, vars.len() as i32,
                                   w_l_scalar.as_ptr() as *const c_void,
                                   w_r_scalar.as_ptr() as *const c_void,
                                   w_o_scalar.as_ptr() as *const c_void,
                                   w_4_scalar.as_ptr() as *const c_void,
                                   w_l_scalar.len() as i32); }*/
       
        println!("start gen proof");
        let now = std::time::Instant::now();
        
        let (proof, pi) = {
            real_circuit.gen_proof::<KZG10<Bls12_381>, G1Affine>(&pp, &pk, b"Merkle tree", &mut prover, &ck, None).unwrap()
        };
        println!("The prove generation time is {:?}", now.elapsed());

        let verifier_data = VerifierData::new(vk, pi.clone());
        let res = verify_proof::<Fr, EdwardsParameters, KZG10<Bls12_381>>(&pp, verifier_data.key.clone(), &proof, &verifier_data.pi, b"Merkle tree");

        //println!("proof: {:?}", proof);
        //println!("public input: {:?}", pi);
        //println!("verifier data: {:?}", verifier_data);
        println!("proof is verified: {}", res.is_ok());
    }
}