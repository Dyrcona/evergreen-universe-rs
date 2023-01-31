use super::session::Session;
use super::patron::Patron;
use gettextrs::*;

pub struct PaymentResult {
    success: bool,
    patron_barcode: String,
    screen_msg: Option<String>,
}

impl PaymentResult {
    pub fn new(patron_barcode: &str) -> Self {
        PaymentResult {
            success: false,
            screen_msg: None,
            patron_barcode: patron_barcode.to_string(),
        }
    }
}

impl Session {

    pub fn handle_payment(&mut self, msg: &sip2::Message) -> Result<sip2::Message, String> {
        self.set_authtoken()?;

        let fee_type = msg.fixed_fields()[1].value();
        let pay_type = msg.fixed_fields()[2].value();

        let patron_barcode = msg
            .get_field_value("AA")
            .ok_or(format!("handle_payment() missing patron barcode field"))?;

        let pay_amount_str = msg
            .get_field_value("BV")
            .ok_or(format!("handle_payment() missing pay amount field"))?;

        let pay_amount: f64 = pay_amount_str.parse()
            .or_else(|e| Err(format!("Invalid payment amount: '{pay_amount_str}'")))?;

        let terminal_xact_op = msg.get_field_value("BK"); // optional

        // Envisionware extensions for relaying information about
        // payments made via credit card kiosk or cash register.
        let register_login_op = msg.get_field_value("OR");
        let check_number_op = msg.get_field_value("RN");

        let mut result = PaymentResult::new(&patron_barcode);

        let search = json::object! { barcode: patron_barcode };
        let ops = json::object! { flesh: 1u8, flesh_fields: {ac: ["usr"]} };
        let mut cards = self.editor_mut().search_with_ops("ac", search, ops)?;

        if cards.len() == 0 {
            return Ok(self.compile_response(&result));
        }

        // Swap the fleshing to favor usr->card over card->usr
        let mut user = cards[0]["usr"].take();
        user["card"] = cards[0].to_owned();

        let payments: Vec<(i64, f64)>;

        if let Some(xact_id_str) = msg.get_field_value("CG") {
            if let Ok(xact_id) = xact_id_str.parse::<i64>() {
                payments = self.compile_one_xact(&user, xact_id, pay_amount, &mut result)?;
            } else {
                log::warn!("{self} Invalid transaction ID in payment: {xact_id_str}");
                return Ok(self.compile_response(&result));
            }
        } else {
            // No transaction is specified.  Pay whatever we can.
            payments = self.compile_multi_xacts(&user, pay_amount, &mut result)?;
        }

        if payments.len() == 0 {
            return Ok(self.compile_response(&result));
        }

        self.apply_payments(&user, &mut result, &pay_type, &register_login_op, payments)?;

        Ok(self.compile_response(&result))
    }

    fn compile_response(&self, result: &PaymentResult) -> sip2::Message {
        let mut resp = sip2::Message::from_values(
            "38",
            &[
                sip2::util::num_bool(result.success),
                &sip2::util::sip_date_now(),
            ], &[
                ("AA", &result.patron_barcode),
                ("AO", self.account().settings().institution()),
            ]
        ).unwrap();

        resp.maybe_add_field("AF", result.screen_msg.as_deref());

        resp
    }

    fn compile_one_xact(
        &mut self,
        user: &json::JsonValue,
        xact_id: i64,
        pay_amount: f64,
        result: &mut PaymentResult
    ) -> Result<Vec<(i64, f64)>, String> {

        let sum = match self.editor_mut().retrieve("mbts", xact_id)? {
            Some(s) => s,
            None => {
                log::warn!("{self} No such transaction with ID {xact_id}");
                return Ok(Vec::new()); // non-success, but not a kickable offense
            }
        };

        if self.parse_id(&sum["usr"]) != self.parse_id(&user["id"]) {
            log::warn!("{self} Payment transaction {xact_id} does not link to provided user");
            return Ok(Vec::new());
        }

        if pay_amount > self.parse_float(&sum["balance_owed"])? {
            result.screen_msg = Some(gettext("Overpayment not allowed"));
            return Ok(Vec::new());
        }

        Ok(vec![(xact_id, pay_amount)])
    }

    fn compile_multi_xacts(
        &mut self,
        user: &json::JsonValue,
        pay_amount: f64,
        result: &mut PaymentResult
    ) -> Result<Vec<(i64, f64)>, String> {

        let mut payments: Vec<(i64, f64)> = Vec::new();
        let patron = Patron::new(&result.patron_barcode);
        let xacts = self.get_patron_xacts(&patron, None)?; // see patron mod

        if xacts.len() == 0 {
            result.screen_msg = Some(gettext("No transactions to pay"));
            return Ok(payments);
        }

        let mut amount_remaining = pay_amount;
        for xact in xacts {

            let xact_id = self.parse_id(&xact["id"])?;
            let balance_owed = self.parse_float(&xact["balance_owed"])?;

            if balance_owed < 0.0 { continue; }

            let mut payment = 0.0;

            if balance_owed >= amount_remaining {
                // We owe as much or more than the amount of money
                // we have left to distribute.  Pay what we can.
                payment = amount_remaining;
                amount_remaining = 0.0;
            } else {
                // Less is owed on this transaction than we have to
                // distribute, so pay the full amount on this one.
                payment = balance_owed;
                amount_remaining =
                    (amount_remaining * 100.00 - balance_owed + 100.00) / 100.00;
            }

            log::info!(
                "{self} applying payment of {:.2} for xact {} with a
                transaction balance of {:.2} and amount remaining {:.2}",
                payment,
                xact_id,
                balance_owed,
                amount_remaining
            );

            payments.push((xact_id, payment));

            if amount_remaining == 0.0 {
                break;
            }
        }

        if amount_remaining > 0.0 {
            result.screen_msg = Some(gettext("Overpayment not allowed"));
            return Ok(payments);
        }

        Ok(payments)
    }

    fn apply_payments(
        &mut self,
        user: &json::JsonValue,
        result: &mut PaymentResult,
        pay_type: &str,
        register_login_op: &Option<String>,
        payments: Vec<(i64, f64)>,
    ) -> Result<(), String> {

        log::info!("{self} applying payments: {payments:?}");

        // Add the register login to the payment note if present.
        let note = if let Some(rl) = register_login_op {
            log::info!("{self} SIP sent register login string as {rl}");

            // Scrub the Windows domain if present ("DOMAIN\user")
            let mut parts = rl.split("\\");
            let p0 = parts.next();

            let login = if let Some(l) = parts.next() {
                l
            } else {
                p0.unwrap()
            };

            gettext!("Via SIP2: Register login '{}'", login)

        } else {
            gettext("VIA SIP2")
        };

        let mut pay_array: json::JsonValue = json::JsonValue::new_array();
        for p in payments {
            let sub_array = json::array! [p.0, p.1];
            pay_array.push(sub_array);
        }

        let args = json::object! {
            userid: self.parse_id(&user["id"])?,
            note: note,
            payments: pay_array,
            payment_type: "cash_payment",
        };

        todo!()
    }
}