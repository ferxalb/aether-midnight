pub fn debit(remaining: &mut u32, cost: u32) -> bool {
    let Some(balance) = remaining.checked_sub(cost) else {
        return false;
    };
    *remaining = balance;
    true
}
